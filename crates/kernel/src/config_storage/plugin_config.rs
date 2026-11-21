//! Plugin-owned configuration in a config set.
//!
//! Two config entity types come from plugins rather than the kernel:
//!
//! - `plugin_status`: whether each installed plugin is enabled, one file per
//!   plugin (`plugin_status.<name>.yml`). Imported first, so a set that enables
//!   a plugin can carry that plugin's configuration.
//! - **plugin config tables**: every table a manifest names under
//!   `[config] tables`, one file per row (`<table>.<primary key>.yml`), after
//!   the kernel's own types. The row is the file: a map of column to value,
//!   converted by Postgres itself (`to_jsonb` on the way out,
//!   `jsonb_populate_record` on the way in), so the kernel needs no per-plugin
//!   code and a plugin's column types round trip exactly.
//!
//! The plugin manifests are read from disk, so this works in the `config`
//! command's own process, with no running server.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sqlx::{AssertSqlSafe, PgPool, Row};

use crate::plugin::PluginInfo;
use crate::plugin::status;

/// A plugin's enabled state, as `plugin_status.<name>.yml`.
///
/// The installed version and the install timestamps are not here: they
/// describe the code on disk and when it arrived, not how the site is
/// configured.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigPluginStatus {
    /// The plugin's machine name.
    pub name: String,
    /// Whether the plugin is enabled.
    pub enabled: bool,
}

/// One row of a plugin config table.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PluginRow {
    /// The table, which is also the entity type.
    pub table: String,
    /// The row's primary key, as it appears in the file name.
    pub id: String,
    /// Column name to value.
    pub row: serde_json::Map<String, serde_json::Value>,
}

/// Every plugin found on disk, for the plugin side of a config set.
#[derive(Debug, Clone, Default)]
pub struct PluginConfigSet {
    plugins: HashMap<String, (PluginInfo, PathBuf)>,
}

impl PluginConfigSet {
    /// The plugins in `dirs`, as the server discovers them at boot.
    pub async fn discover(dirs: &[PathBuf]) -> Self {
        Self::from_discovered(crate::plugin::PluginRuntime::discover_plugins(dirs).await)
    }

    /// Wrap an already discovered set of plugins.
    pub fn from_discovered(plugins: HashMap<String, (PluginInfo, PathBuf)>) -> Self {
        Self { plugins }
    }

    /// The manifest of `name`, when it is on disk.
    pub fn info(&self, name: &str) -> Option<&PluginInfo> {
        self.plugins.get(name).map(|(info, _)| info)
    }

    /// Every declared config table, with the plugin that owns it, plugins in
    /// migration dependency order and each plugin's tables in declared order.
    ///
    /// A plugin's migrations may refer to another's tables, and their rows may
    /// too; the order that is safe for one is safe for the other.
    pub fn tables(&self) -> Vec<(String, String)> {
        let infos: HashMap<String, PluginInfo> = self
            .plugins
            .iter()
            .map(|(name, (info, _))| (name.clone(), info.clone()))
            .collect();
        let order =
            crate::plugin::migration::resolve_migration_order(&infos).unwrap_or_else(|_| {
                let mut names: Vec<String> = infos.keys().cloned().collect();
                names.sort();
                names
            });
        let mut tables = Vec::new();
        for name in order {
            if let Some(info) = infos.get(&name) {
                for table in &info.config.tables {
                    tables.push((name.clone(), table.clone()));
                }
            }
        }
        tables
    }

    /// The plugin that declares `table`, if any.
    pub fn owner(&self, table: &str) -> Option<&str> {
        self.plugins
            .iter()
            .find(|(_, (info, _))| info.config.tables.iter().any(|t| t == table))
            .map(|(name, _)| name.as_str())
    }

    /// Declarations the kernel will not act on, as `(table, why)`.
    ///
    /// A config table must be one the plugin's own migrations create, must not
    /// share a name with a kernel config entity type, and must not be claimed
    /// by two plugins. Import refuses a set while any of these stand; export
    /// skips the table and warns.
    pub fn problems(&self, kernel_types: &[&str]) -> Vec<(String, String)> {
        let mut problems = Vec::new();
        let mut claimed: HashMap<&str, &str> = HashMap::new();
        let mut names: Vec<&String> = self.plugins.keys().collect();
        names.sort();
        for name in names {
            let (info, dir) = &self.plugins[name];
            if info.config.tables.is_empty() {
                continue;
            }
            let owned = migration_created_tables(info, dir);
            for table in &info.config.tables {
                if kernel_types.contains(&table.as_str()) {
                    problems.push((
                        table.clone(),
                        format!(
                            "plugin '{name}' declares config table '{table}', which is the \
                             name of a kernel config entity type"
                        ),
                    ));
                } else if !owned.contains(table) {
                    problems.push((
                        table.clone(),
                        format!(
                            "plugin '{name}' declares config table '{table}', which none of \
                             its migrations create"
                        ),
                    ));
                }
                if let Some(other) = claimed.insert(table.as_str(), name.as_str()) {
                    problems.push((
                        table.clone(),
                        format!(
                            "config table '{table}' is declared by both '{other}' and '{name}'"
                        ),
                    ));
                }
            }
        }
        problems
    }

    /// Run the pending migrations of every enabled plugin on disk, as the
    /// server does at boot, so the tables of a plugin the import has just
    /// enabled exist before its rows are written.
    pub async fn migrate_enabled(&self, pool: &PgPool) -> Result<()> {
        let enabled: HashSet<String> = status::get_enabled_names(pool).await?.into_iter().collect();
        let to_migrate: HashMap<String, (PluginInfo, PathBuf)> = self
            .plugins
            .iter()
            .filter(|(name, _)| enabled.contains(*name))
            .map(|(name, entry)| (name.clone(), entry.clone()))
            .collect();
        crate::plugin::migration::run_all_pending_migrations(pool, &to_migrate)
            .await
            .context("failed to run the migrations of the enabled plugins")?;
        Ok(())
    }
}

/// Every table a plugin's migrations create.
fn migration_created_tables(info: &PluginInfo, dir: &Path) -> HashSet<String> {
    let mut tables = HashSet::new();
    for file in &info.migrations.files {
        if let Ok(sql) = std::fs::read_to_string(dir.join(file)) {
            tables.extend(crate::plugin::db_policy::extract_created_tables(&sql));
        }
    }
    tables
}

/// Quote an identifier for interpolation into SQL.
fn ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// A table's writable columns, primary key and other unique keys, read from
/// the catalog.
#[derive(Debug, Clone)]
pub struct TableShape {
    /// Every column that is neither dropped nor generated.
    pub columns: Vec<String>,
    /// The primary key's columns, in key order.
    pub primary_key: Vec<String>,
    /// Each other unique constraint's columns.
    pub unique: Vec<Vec<String>>,
}

/// The shape of `table`, or `None` when it does not exist (yet).
pub async fn table_shape(pool: &PgPool, table: &str) -> Result<Option<TableShape>> {
    let exists: bool = sqlx::query_scalar("SELECT to_regclass($1) IS NOT NULL")
        .bind(ident(table))
        .fetch_one(pool)
        .await?;
    if !exists {
        return Ok(None);
    }
    let columns: Vec<String> = sqlx::query_scalar(
        "SELECT attname::text FROM pg_attribute \
         WHERE attrelid = to_regclass($1) AND attnum > 0 AND NOT attisdropped \
           AND attgenerated = '' \
         ORDER BY attnum",
    )
    .bind(ident(table))
    .fetch_all(pool)
    .await?;
    let rows = sqlx::query(
        "SELECT c.contype::text AS kind, \
                array_agg(a.attname::text ORDER BY k.ord) AS cols \
         FROM pg_constraint c \
         CROSS JOIN LATERAL unnest(c.conkey) WITH ORDINALITY AS k(attnum, ord) \
         JOIN pg_attribute a ON a.attrelid = c.conrelid AND a.attnum = k.attnum \
         WHERE c.conrelid = to_regclass($1) AND c.contype IN ('p', 'u') \
         GROUP BY c.oid, c.contype \
         ORDER BY c.contype, c.oid",
    )
    .bind(ident(table))
    .fetch_all(pool)
    .await?;
    let mut primary_key = Vec::new();
    let mut unique = Vec::new();
    for row in rows {
        let kind: String = row.get("kind");
        let cols: Vec<String> = row.get("cols");
        if kind == "p" {
            primary_key = cols;
        } else {
            unique.push(cols);
        }
    }
    if primary_key.is_empty() {
        anyhow::bail!("table '{table}' has no primary key, so its rows cannot be named");
    }
    Ok(Some(TableShape {
        columns,
        primary_key,
        unique,
    }))
}

/// One primary key value as a file name component.
///
/// Percent-encodes everything but ASCII letters, digits, `-` and `_`, so the
/// components can be joined with `.` and two different keys never share a
/// name. An empty value is `%`, which no encoding produces.
fn encode_key_part(value: &serde_json::Value) -> String {
    let raw = match value {
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    if raw.is_empty() {
        return "%".to_string();
    }
    let mut out = String::with_capacity(raw.len());
    for b in raw.bytes() {
        if b.is_ascii_alphanumeric() || b == b'-' || b == b'_' {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// The file name id of a row: its primary key values, encoded, joined by `.`.
pub fn row_id(shape: &TableShape, row: &serde_json::Map<String, serde_json::Value>) -> String {
    shape
        .primary_key
        .iter()
        .map(|col| encode_key_part(row.get(col).unwrap_or(&serde_json::Value::Null)))
        .collect::<Vec<_>>()
        .join(".")
}

/// Every row of `table`, ordered by primary key. Empty when the table does
/// not exist.
pub async fn list_rows(pool: &PgPool, table: &str) -> Result<Vec<PluginRow>> {
    let Some(shape) = table_shape(pool, table).await? else {
        return Ok(Vec::new());
    };
    let order = shape
        .primary_key
        .iter()
        .map(|c| format!("t.{}", ident(c)))
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "SELECT to_jsonb(t) AS row FROM {} t ORDER BY {order}",
        ident(table)
    );
    let rows: Vec<serde_json::Value> = sqlx::query_scalar(AssertSqlSafe(sql))
        .fetch_all(pool)
        .await
        .with_context(|| format!("failed to read config table '{table}'"))?;
    let mut out = Vec::with_capacity(rows.len());
    for value in rows {
        let serde_json::Value::Object(mut row) = value else {
            continue;
        };
        // Only the columns an insert can write.
        row.retain(|k, _| shape.columns.contains(k));
        out.push(PluginRow {
            table: table.to_string(),
            id: row_id(&shape, &row),
            row,
        });
    }
    Ok(out)
}

/// Why `row` cannot be written to a table of `shape`, if it cannot.
///
/// Every column it names must exist, the primary key must be complete, and
/// Postgres must be able to read each value as its column's type.
pub async fn check_row(
    pool: &PgPool,
    table: &str,
    shape: &TableShape,
    row: &serde_json::Map<String, serde_json::Value>,
) -> std::result::Result<(), String> {
    let unknown: Vec<&String> = row.keys().filter(|k| !shape.columns.contains(k)).collect();
    if !unknown.is_empty() {
        return Err(format!(
            "table '{table}' has no column {}",
            unknown
                .iter()
                .map(|k| format!("'{k}'"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if let Some(missing) = shape.primary_key.iter().find(|c| !row.contains_key(*c)) {
        return Err(format!("the primary key column '{missing}' is missing"));
    }
    let sql = format!(
        "SELECT 1 FROM jsonb_populate_record(NULL::{}, $1)",
        ident(table)
    );
    sqlx::query(AssertSqlSafe(sql))
        .bind(serde_json::Value::Object(row.clone()))
        .fetch_one(pool)
        .await
        .map(|_| ())
        .map_err(|e| format!("a value does not fit its column: {e}"))
}

/// Write `row` into `table`: insert it, or update the row with its primary key.
///
/// A row whose other unique key (an image style's `name`, say) is already held
/// under a different primary key takes that row over: the existing row is
/// given this row's key first. The set is authoritative about identity, which
/// is what lets a fresh install's seeded rows give way to the exported ones.
pub async fn save_row(pool: &PgPool, row: &PluginRow) -> Result<()> {
    let table = row.table.as_str();
    let shape = table_shape(pool, table)
        .await?
        .with_context(|| format!("table '{table}' does not exist: is its plugin enabled?"))?;
    if let Err(e) = check_row(pool, table, &shape, &row.row).await {
        anyhow::bail!("{e}");
    }
    let value = serde_json::Value::Object(row.row.clone());
    let present: Vec<&String> = shape
        .columns
        .iter()
        .filter(|c| row.row.contains_key(*c))
        .collect();
    let pk_match = |lhs: &str, rhs: &str| -> String {
        shape
            .primary_key
            .iter()
            .map(|c| format!("{lhs}.{c} = {rhs}.{c}", c = ident(c)))
            .collect::<Vec<_>>()
            .join(" AND ")
    };

    let mut tx = pool.begin().await?;

    for key in &shape.unique {
        if !key.iter().all(|c| row.row.contains_key(c)) {
            continue;
        }
        let set = shape
            .primary_key
            .iter()
            .map(|c| format!("{c} = r.{c}", c = ident(c)))
            .collect::<Vec<_>>()
            .join(", ");
        let same_key = key
            .iter()
            .map(|c| format!("t.{c} = r.{c}", c = ident(c)))
            .collect::<Vec<_>>()
            .join(" AND ");
        let sql = format!(
            "UPDATE {tbl} AS t SET {set} FROM jsonb_populate_record(NULL::{tbl}, $1) AS r \
             WHERE {same_key} AND NOT ({pk})",
            tbl = ident(table),
            pk = pk_match("t", "r"),
        );
        sqlx::query(AssertSqlSafe(sql))
            .bind(&value)
            .execute(&mut *tx)
            .await
            .with_context(|| format!("failed to re-key the row holding this row's {key:?}"))?;
    }

    let cols = present
        .iter()
        .map(|c| ident(c))
        .collect::<Vec<_>>()
        .join(", ");
    let conflict = shape
        .primary_key
        .iter()
        .map(|c| ident(c))
        .collect::<Vec<_>>()
        .join(", ");
    let updates: Vec<String> = present
        .iter()
        .filter(|c| !shape.primary_key.contains(c))
        .map(|c| format!("{c} = EXCLUDED.{c}", c = ident(c)))
        .collect();
    let on_conflict = if updates.is_empty() {
        "DO NOTHING".to_string()
    } else {
        format!("DO UPDATE SET {}", updates.join(", "))
    };
    let sql = format!(
        "INSERT INTO {tbl} ({cols}) SELECT {cols} FROM jsonb_populate_record(NULL::{tbl}, $1) \
         ON CONFLICT ({conflict}) {on_conflict}",
        tbl = ident(table),
    );
    sqlx::query(AssertSqlSafe(sql))
        .bind(&value)
        .execute(&mut *tx)
        .await
        .with_context(|| format!("failed to write the row into '{table}'"))?;

    tx.commit().await?;
    Ok(())
}

/// Every installed plugin's enabled state, by name.
pub async fn list_statuses(pool: &PgPool) -> Result<Vec<ConfigPluginStatus>> {
    Ok(status::get_all_statuses(pool)
        .await?
        .into_iter()
        .map(|s| ConfigPluginStatus {
            name: s.name,
            enabled: s.status == status::STATUS_ENABLED,
        })
        .collect())
}

/// One plugin's enabled state, if it is installed.
pub async fn load_status(pool: &PgPool, name: &str) -> Result<Option<ConfigPluginStatus>> {
    Ok(list_statuses(pool)
        .await?
        .into_iter()
        .find(|s| s.name == name))
}

/// Install or update a plugin's enabled state.
///
/// A plugin not yet installed is installed with the version its manifest on
/// disk declares (empty when the caller has no manifests). An installed
/// plugin keeps its version; only its status changes, and only if it differs.
pub async fn save_status(pool: &PgPool, entry: &ConfigPluginStatus, version: &str) -> Result<()> {
    let now = chrono::Utc::now().timestamp();
    let status = if entry.enabled {
        status::STATUS_ENABLED
    } else {
        status::STATUS_DISABLED
    };
    sqlx::query(
        "INSERT INTO plugin_status (name, status, version, installed_at, updated_at) \
         VALUES ($1, $2, $3, $4, $4) \
         ON CONFLICT (name) DO UPDATE SET status = EXCLUDED.status, updated_at = EXCLUDED.updated_at \
         WHERE plugin_status.status <> EXCLUDED.status",
    )
    .bind(&entry.name)
    .bind(status)
    .bind(version)
    .bind(now)
    .execute(pool)
    .await
    .with_context(|| format!("failed to save the status of plugin '{}'", entry.name))?;
    Ok(())
}

/// Delete a plugin's status row. Its tables and data stay.
pub async fn delete_status(pool: &PgPool, name: &str) -> Result<bool> {
    let result = sqlx::query("DELETE FROM plugin_status WHERE name = $1")
        .bind(name)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() > 0)
}

/// The enabled set after an import applies `incoming` on top of what the
/// database holds now.
pub async fn enabled_after(
    pool: &PgPool,
    incoming: &[ConfigPluginStatus],
) -> Result<BTreeMap<String, bool>> {
    let mut enabled: BTreeMap<String, bool> = list_statuses(pool)
        .await?
        .into_iter()
        .map(|s| (s.name, s.enabled))
        .collect();
    for s in incoming {
        enabled.insert(s.name.clone(), s.enabled);
    }
    Ok(enabled)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn shape(pk: &[&str]) -> TableShape {
        TableShape {
            columns: pk.iter().map(|s| s.to_string()).collect(),
            primary_key: pk.iter().map(|s| s.to_string()).collect(),
            unique: Vec::new(),
        }
    }

    #[test]
    fn a_uuid_key_is_its_own_id() {
        let row = serde_json::json!({"id": "0193a5b0-7000-7000-8000-000000000001"});
        let id = row_id(&shape(&["id"]), row.as_object().unwrap());
        assert_eq!(id, "0193a5b0-7000-7000-8000-000000000001");
    }

    #[test]
    fn a_composite_key_joins_with_dots_and_escapes_its_parts() {
        let row = serde_json::json!({
            "entity_type": "variable",
            "entity_id": "site.name",
            "language": "",
        });
        let id = row_id(
            &shape(&["entity_type", "entity_id", "language"]),
            row.as_object().unwrap(),
        );
        assert_eq!(id, "variable.site%2Ename.%");
    }

    #[test]
    fn an_escaped_id_is_a_safe_file_name() {
        let row = serde_json::json!({"source": "/old/path?x=1", "n": 3});
        let id = row_id(&shape(&["source", "n"]), row.as_object().unwrap());
        assert_eq!(id, "%2Fold%2Fpath%3Fx%3D1.3");
        assert!(!id.contains('/') && !id.contains(".."));
    }
}
