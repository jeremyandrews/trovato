//! Registry of the permissions plugins declare from `tap_perm`.
//!
//! Built at boot from the tap's JSON output, exactly like
//! [`crate::menu::MenuRegistry`] and [`crate::assistant::AssistantRegistry`],
//! and for a reason those two share: a declaration is a constant, so it is
//! collected once rather than queried during a request.
//!
//! # Why this one is also written to the database
//!
//! The other two registries live only in memory, because only the running
//! server consults them. A permission has a second consumer that is not the
//! running server: `config import` validates a `role.*.yml` file's `permissions`
//! list, and it runs in its own process with a `PgPool` and no `AppState`. A
//! registry it cannot read is a registry that cannot make a plugin's permission
//! nameable in a config file, which is most of the point.
//!
//! So the boot dispatch writes what it found to `plugin_permission`, and both
//! the grid and the import read it back from there. The table is a cache of
//! declarations, never a grant: `role_permissions` remains the only place a
//! permission is actually held by anyone.
//!
//! # A disabled plugin keeps its rows
//!
//! Only enabled plugins are loaded, so only they can be dispatched, and a
//! refresh replaces the rows of the plugins that answered. Rows belonging to a
//! plugin that did not answer are left alone rather than deleted. That is
//! deliberate: a site that disables a plugin still has roles holding its
//! permissions, and deleting the declarations would make the grid stop
//! rendering them, which is exactly the condition that made saving the grid
//! destroy them. Keeping them costs a row and keeps the screen honest.

use std::collections::HashSet;

use anyhow::{Context, Result};
use sqlx::PgPool;
use tracing::warn;
use trovato_sdk::types::PermissionDefinition;

/// Longest permission name that fits the column.
const MAX_NAME_LEN: usize = 255;

/// A permission a plugin declared, with the plugin that declared it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct PluginPermission {
    /// The permission string a role holds and a route checks.
    pub name: String,
    /// The plugin that declared it.
    pub plugin: String,
    /// Human-readable description, shown in the grid.
    pub description: String,
}

/// Why one declaration was dropped.
#[derive(Debug, Clone)]
pub struct PermissionRejection {
    pub plugin: String,
    pub name: String,
    pub reason: String,
}

/// Every permission declared by the plugins that answered `tap_perm`.
#[derive(Debug, Default)]
pub struct PluginPermissionRegistry {
    permissions: Vec<PluginPermission>,
    rejections: Vec<PermissionRejection>,
}

impl PluginPermissionRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Parse the dispatch results, dropping what cannot be stored.
    ///
    /// A rejection is never fatal: one plugin's malformed declaration must not
    /// stop a site booting, so it is recorded, logged once, and skipped.
    pub fn from_tap_results(results: Vec<(String, String)>) -> Self {
        let mut registry = Self::new();
        let mut seen: HashSet<String> = HashSet::new();

        for (plugin, json) in results {
            // `#[plugin_tap]` serializes the return value, so an array arrives
            // as a JSON array — but a String-returning tap arrives as a JSON
            // string wrapping one, the same double encoding the view and api
            // paths already accept (G-VIEW-OUTPUT-JSON-ENCODED).
            let parsed = serde_json::from_str::<serde_json::Value>(&json)
                .map(|value| match value {
                    serde_json::Value::String(ref inner) => {
                        serde_json::from_str::<serde_json::Value>(inner).unwrap_or(value)
                    }
                    other => other,
                })
                .and_then(serde_json::from_value::<Vec<PermissionDefinition>>);

            let definitions = match parsed {
                Ok(definitions) => definitions,
                Err(e) => {
                    registry.reject(&plugin, "<unparsed>", format!("output did not parse: {e}"));
                    continue;
                }
            };

            for definition in definitions {
                let name = definition.name.trim().to_string();
                if name.is_empty() {
                    registry.reject(&plugin, "<unnamed>", "permission name is empty".to_string());
                    continue;
                }
                if name.len() > MAX_NAME_LEN {
                    registry.reject(
                        &plugin,
                        &name,
                        format!("permission name is longer than {MAX_NAME_LEN} bytes"),
                    );
                    continue;
                }
                // The kernel's own names win. A plugin redeclaring one would
                // otherwise relabel a kernel permission in the grid and imply
                // the plugin owns it.
                if crate::models::role::KERNEL_PERMISSIONS.contains(&name.as_str()) {
                    registry.reject(
                        &plugin,
                        &name,
                        "the kernel already defines this permission".to_string(),
                    );
                    continue;
                }
                if !seen.insert(name.clone()) {
                    registry.reject(
                        &plugin,
                        &name,
                        "another plugin already declared this permission".to_string(),
                    );
                    continue;
                }

                registry.permissions.push(PluginPermission {
                    name,
                    plugin: plugin.clone(),
                    description: definition.description,
                });
            }
        }

        for rejection in &registry.rejections {
            warn!(
                plugin = %rejection.plugin,
                permission = %rejection.name,
                reason = %rejection.reason,
                "dropping an invalid plugin permission declaration"
            );
        }

        registry
    }

    fn reject(&mut self, plugin: &str, name: &str, reason: String) {
        self.rejections.push(PermissionRejection {
            plugin: plugin.to_string(),
            name: name.to_string(),
            reason,
        });
    }

    pub fn permissions(&self) -> &[PluginPermission] {
        &self.permissions
    }

    pub fn rejections(&self) -> &[PermissionRejection] {
        &self.rejections
    }

    pub fn is_empty(&self) -> bool {
        self.permissions.is_empty()
    }

    pub fn len(&self) -> usize {
        self.permissions.len()
    }
}

/// Replace the stored declarations of every plugin in this registry.
///
/// Scoped to the plugins that answered: a plugin that is disabled, or that
/// failed to load, keeps whatever it declared last time. See the module docs.
pub async fn persist(pool: &PgPool, registry: &PluginPermissionRegistry) -> Result<()> {
    let plugins: HashSet<&str> = registry
        .permissions()
        .iter()
        .map(|p| p.plugin.as_str())
        .collect();

    let mut tx = pool.begin().await.context("begin permission refresh")?;

    for plugin in &plugins {
        sqlx::query("DELETE FROM plugin_permission WHERE plugin = $1")
            .bind(plugin)
            .execute(&mut *tx)
            .await
            .context("clear a plugin's declared permissions")?;
    }

    for permission in registry.permissions() {
        sqlx::query(
            "INSERT INTO plugin_permission (name, plugin, description)
             VALUES ($1, $2, $3)
             ON CONFLICT (name) DO UPDATE
                SET plugin = EXCLUDED.plugin, description = EXCLUDED.description",
        )
        .bind(&permission.name)
        .bind(&permission.plugin)
        .bind(&permission.description)
        .execute(&mut *tx)
        .await
        .context("store a declared permission")?;
    }

    tx.commit().await.context("commit permission refresh")?;
    Ok(())
}

/// Every permission any plugin has declared, ordered by plugin then name.
///
/// The grid renders these beside the kernel's own, and `config import`
/// validates against them. Both need the same list, so it lives here.
pub async fn load_all(pool: &PgPool) -> Result<Vec<PluginPermission>> {
    let rows = sqlx::query_as::<_, (String, String, String)>(
        "SELECT name, plugin, description FROM plugin_permission ORDER BY plugin, name",
    )
    .fetch_all(pool)
    .await
    .context("failed to list declared plugin permissions")?;

    Ok(rows
        .into_iter()
        .map(|(name, plugin, description)| PluginPermission {
            name,
            plugin,
            description,
        })
        .collect())
}
