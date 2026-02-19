//! Plugin migration runner.
//!
//! Reads SQL migration files declared in a plugin's `info.toml`, tracks
//! which have been applied in the `plugin_migration` table, and runs
//! pending migrations inside a per-plugin transaction.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::Path;

use anyhow::{Result, bail};
use sqlx::{PgPool, Row};
use tracing::{debug, info, warn};

use super::error::PluginError;
use super::info_parser::PluginInfo;

/// Get the list of already-applied migration names for a plugin.
pub async fn get_applied_migrations(pool: &PgPool, plugin_name: &str) -> Result<Vec<String>> {
    let rows =
        sqlx::query("SELECT migration FROM plugin_migration WHERE plugin = $1 ORDER BY migration")
            .bind(plugin_name)
            .fetch_all(pool)
            .await?;

    Ok(rows.iter().map(|r| r.get("migration")).collect())
}

/// Run pending migrations for a single plugin.
///
/// Reads SQL files from disk, skips already-applied ones, and runs pending
/// migrations in a single transaction. Records each applied migration in
/// `plugin_migration`.
///
/// Returns the list of newly applied migration names.
pub async fn run_plugin_migrations(
    pool: &PgPool,
    plugin_name: &str,
    info: &PluginInfo,
    plugin_dir: &Path,
) -> Result<Vec<String>> {
    if info.migrations.files.is_empty() {
        return Ok(Vec::new());
    }

    let applied = get_applied_migrations(pool, plugin_name).await?;
    let applied_set: HashSet<&str> = applied.iter().map(|s| s.as_str()).collect();

    // Collect pending migrations
    let pending: Vec<&str> = info
        .migrations
        .files
        .iter()
        .map(|s| s.as_str())
        .filter(|f| !applied_set.contains(*f))
        .collect();

    if pending.is_empty() {
        debug!(plugin = plugin_name, "no pending migrations");
        return Ok(Vec::new());
    }

    info!(
        plugin = plugin_name,
        count = pending.len(),
        "running pending migrations"
    );

    // Run all pending migrations in a single transaction (atomic per plugin)
    let mut tx = pool.begin().await?;
    let now = chrono::Utc::now().timestamp();
    let mut newly_applied = Vec::new();

    for migration_file in &pending {
        let sql_path = plugin_dir.join(migration_file);

        if !sql_path.exists() {
            return Err(PluginError::MigrationFileNotFound {
                plugin: plugin_name.to_string(),
                path: sql_path.display().to_string(),
            }
            .into());
        }

        let sql = std::fs::read_to_string(&sql_path).map_err(|e| PluginError::MigrationFailed {
            plugin: plugin_name.to_string(),
            migration: migration_file.to_string(),
            details: format!("failed to read file: {e}"),
        })?;

        debug!(
            plugin = plugin_name,
            migration = migration_file,
            "executing migration"
        );

        // Use raw_sql instead of query() because migration files contain
        // multiple SQL statements. query() uses prepared statements which
        // only support a single statement per call.
        sqlx::raw_sql(&sql)
            .execute(&mut *tx)
            .await
            .map_err(|e| PluginError::MigrationFailed {
                plugin: plugin_name.to_string(),
                migration: migration_file.to_string(),
                details: e.to_string(),
            })?;

        // Record in plugin_migration
        sqlx::query(
            "INSERT INTO plugin_migration (plugin, migration, applied_at) VALUES ($1, $2, $3)",
        )
        .bind(plugin_name)
        .bind(*migration_file)
        .bind(now)
        .execute(&mut *tx)
        .await?;

        newly_applied.push(migration_file.to_string());
    }

    tx.commit().await?;

    info!(
        plugin = plugin_name,
        applied = newly_applied.len(),
        "migrations complete"
    );

    Ok(newly_applied)
}

/// Resolve migration order across all plugins using dependency graph.
///
/// Uses Kahn's algorithm, ordering by `migrations.depends_on` merged with
/// plugin `dependencies`.
///
/// # Panics
///
/// Panics if a plugin key is missing from the `in_degree` map. This cannot
/// happen because every key from `plugins` is inserted during initialization
/// before the graph-building loop.
pub fn resolve_migration_order(plugins: &HashMap<String, PluginInfo>) -> Result<Vec<String>> {
    // Build combined dependency graph: for migration ordering, a plugin's
    // migration depends on both its `dependencies` and `migrations.depends_on`.
    let mut in_degree: HashMap<&str, usize> = HashMap::new();
    let mut dependents: HashMap<&str, Vec<&str>> = HashMap::new();

    for name in plugins.keys() {
        in_degree.insert(name, 0);
        dependents.entry(name.as_str()).or_default();
    }

    for (name, info) in plugins {
        // Combine plugin dependencies and migration depends_on.
        // Both are hard errors if the referenced plugin is not in the set —
        // this matches resolve_load_order behavior in dependency.rs.
        let mut all_deps: HashSet<&str> = HashSet::new();
        for dep in &info.dependencies {
            if !plugins.contains_key(dep) {
                bail!("plugin '{name}': depends on '{dep}' which is not in the enabled plugin set");
            }
            all_deps.insert(dep);
        }
        for dep in &info.migrations.depends_on {
            if !plugins.contains_key(dep) {
                bail!("plugin '{name}': migration depends_on '{dep}' which is not available");
            }
            all_deps.insert(dep);
        }

        for dep in all_deps {
            // Key guaranteed present: inserted in initialization loop above
            #[allow(clippy::expect_used)]
            {
                *in_degree
                    .get_mut(name.as_str())
                    .expect("plugin key missing from in_degree map") += 1;
            }
            dependents.entry(dep).or_default().push(name);
        }
    }

    // Kahn's algorithm with deterministic ordering.
    // Use a sorted BTreeSet for newly-unblocked nodes so that independent
    // plugins always appear in alphabetical order across runs.
    let mut result = Vec::with_capacity(plugins.len());
    let mut queue: VecDeque<&str> = VecDeque::new();

    // Seed with zero-in-degree nodes in sorted order
    let mut roots: Vec<&str> = in_degree
        .iter()
        .filter(|(_, d)| **d == 0)
        .map(|(name, _)| *name)
        .collect();
    roots.sort();
    queue.extend(roots);

    while let Some(plugin) = queue.pop_front() {
        result.push(plugin.to_string());

        if let Some(deps) = dependents.get(plugin) {
            // Collect newly-unblocked dependents and sort for determinism
            let mut newly_ready: Vec<&str> = Vec::new();
            for dependent in deps {
                // Key guaranteed present: inserted in initialization loop above
                #[allow(clippy::expect_used)]
                let degree = in_degree
                    .get_mut(*dependent)
                    .expect("dependent key missing from in_degree map");
                *degree -= 1;
                if *degree == 0 {
                    newly_ready.push(*dependent);
                }
            }
            newly_ready.sort();
            queue.extend(newly_ready);
        }
    }

    if result.len() != plugins.len() {
        let loaded: HashSet<_> = result.iter().map(|s| s.as_str()).collect();
        let in_cycle: Vec<_> = plugins
            .keys()
            .filter(|k| !loaded.contains(k.as_str()))
            .cloned()
            .collect();

        bail!(
            "circular migration dependency detected involving plugins: {}",
            in_cycle.join(", ")
        );
    }

    Ok(result)
}

/// Run all pending migrations for all plugins in dependency order.
pub async fn run_all_pending_migrations(
    pool: &PgPool,
    plugins: &HashMap<String, (PluginInfo, std::path::PathBuf)>,
) -> Result<HashMap<String, Vec<String>>> {
    // Build a PluginInfo-only map for ordering
    let info_map: HashMap<String, PluginInfo> = plugins
        .iter()
        .map(|(name, (info, _))| (name.clone(), info.clone()))
        .collect();

    let order = resolve_migration_order(&info_map)?;

    let mut results = HashMap::new();

    for plugin_name in &order {
        if let Some((info, dir)) = plugins.get(plugin_name) {
            if info.migrations.files.is_empty() {
                continue;
            }

            match run_plugin_migrations(pool, plugin_name, info, dir).await {
                Ok(applied) => {
                    if !applied.is_empty() {
                        results.insert(plugin_name.clone(), applied);
                    }
                }
                Err(e) => {
                    warn!(plugin = %plugin_name, error = %e, "migration failed");
                    return Err(e);
                }
            }
        }
    }

    Ok(results)
}

#[cfg(test)]
// Tests are allowed to use unwrap/expect freely.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::plugin::info_parser::{MigrationConfig, TapConfig};

    fn make_plugin(name: &str, deps: Vec<&str>, migration_deps: Vec<&str>) -> PluginInfo {
        PluginInfo {
            name: name.to_string(),
            description: format!("{name} plugin"),
            version: "1.0.0".to_string(),
            default_enabled: true,
            dependencies: deps.into_iter().map(String::from).collect(),
            taps: TapConfig::default(),
            migrations: MigrationConfig {
                files: Vec::new(),
                depends_on: migration_deps.into_iter().map(String::from).collect(),
            },
        }
    }

    #[test]
    fn migration_order_no_deps() {
        let mut plugins = HashMap::new();
        plugins.insert("a".to_string(), make_plugin("a", vec![], vec![]));
        plugins.insert("b".to_string(), make_plugin("b", vec![], vec![]));

        let order = resolve_migration_order(&plugins).unwrap();
        assert_eq!(order.len(), 2);
    }

    #[test]
    fn migration_order_deterministic() {
        let mut plugins = HashMap::new();
        plugins.insert("zebra".to_string(), make_plugin("zebra", vec![], vec![]));
        plugins.insert("alpha".to_string(), make_plugin("alpha", vec![], vec![]));
        plugins.insert("middle".to_string(), make_plugin("middle", vec![], vec![]));

        for _ in 0..10 {
            let order = resolve_migration_order(&plugins).unwrap();
            assert_eq!(order, vec!["alpha", "middle", "zebra"]);
        }
    }

    #[test]
    fn migration_order_respects_depends_on() {
        let mut plugins = HashMap::new();
        plugins.insert("base".to_string(), make_plugin("base", vec![], vec![]));
        plugins.insert("ext".to_string(), make_plugin("ext", vec![], vec!["base"]));

        let order = resolve_migration_order(&plugins).unwrap();
        let base_pos = order.iter().position(|x| x == "base").unwrap();
        let ext_pos = order.iter().position(|x| x == "ext").unwrap();
        assert!(base_pos < ext_pos);
    }

    #[test]
    fn migration_order_merges_plugin_deps_and_migration_deps() {
        let mut plugins = HashMap::new();
        plugins.insert("a".to_string(), make_plugin("a", vec![], vec![]));
        plugins.insert("b".to_string(), make_plugin("b", vec![], vec![]));
        // c depends on a (plugin dep) and b (migration dep)
        plugins.insert("c".to_string(), make_plugin("c", vec!["a"], vec!["b"]));

        let order = resolve_migration_order(&plugins).unwrap();
        let a_pos = order.iter().position(|x| x == "a").unwrap();
        let b_pos = order.iter().position(|x| x == "b").unwrap();
        let c_pos = order.iter().position(|x| x == "c").unwrap();

        assert!(a_pos < c_pos);
        assert!(b_pos < c_pos);
    }

    #[test]
    fn migration_order_detects_cycle() {
        let mut plugins = HashMap::new();
        plugins.insert("a".to_string(), make_plugin("a", vec![], vec!["b"]));
        plugins.insert("b".to_string(), make_plugin("b", vec![], vec!["a"]));

        let result = resolve_migration_order(&plugins);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("circular"));
    }

    #[test]
    fn migration_order_missing_migration_dep() {
        let mut plugins = HashMap::new();
        plugins.insert("a".to_string(), make_plugin("a", vec![], vec!["missing"]));

        let result = resolve_migration_order(&plugins);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("missing"));
    }

    #[test]
    fn migration_order_missing_plugin_dep_is_hard_error() {
        // Plugin "a" declares dependencies = ["missing_plugin"] but
        // missing_plugin is not in the set. This must be a hard error,
        // not silently skipped.
        let mut plugins = HashMap::new();
        plugins.insert(
            "a".to_string(),
            make_plugin("a", vec!["missing_plugin"], vec![]),
        );

        let result = resolve_migration_order(&plugins);
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("missing_plugin"),
            "error should name the missing dep: {msg}"
        );
    }
}
