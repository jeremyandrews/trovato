//! Per-plugin database-scoping policy (WASM-2 / D-19, Option A).
//!
//! The kernel confines a plugin's structured database access to an **effective
//! table allowlist** and gates the raw-SQL host functions behind a declared
//! capability. Both are derived once at plugin load from the plugin's manifest
//! (`[capabilities]`) and its migration SQL, cached on
//! [`crate::plugin::CompiledPlugin`], and threaded into
//! [`crate::plugin::PluginState`] so the `db` host functions
//! (`crates/kernel/src/host/db.rs`) can enforce them per call.
//!
//! # Effective table allowlist (D-19 §1)
//!
//! ```text
//! allowlist = { tables the plugin's own migrations CREATE }
//!           ∪ { explicit db_tables in the manifest }
//! ```
//!
//! Migration-owned tables are parsed from the `CREATE TABLE` statements in the
//! plugin's declared `migrations/*.sql` files ([`extract_created_tables`],
//! handling `IF NOT EXISTS`, schema qualifiers, and quoted identifiers). The
//! manifest `db_tables` list is the explicit extension for tables a plugin
//! touches but does not own through its own migrations.
//!
//! # Raw SQL (D-19 §3)
//!
//! `query-raw` / `execute-raw` are **gated, not parsed**: a plugin may call them
//! only if it declared `raw_sql = true`. The kernel deliberately does not attempt
//! to SQL-parse raw statements against the allowlist — the declared `raw_sql`
//! capability is the documented, auditable escape hatch, and holding it weakens
//! the table guarantee for that plugin (the SQLI-1 surface).
//!
//! # Deny semantics (D-19 §4)
//!
//! `capabilities: None` yields an empty allowlist with `raw_sql = false` — deny.
//! In practice such a plugin never reaches these functions anyway: the WASM-1
//! per-plugin linker refuses to link the `db` interface for a plugin that does
//! not declare it. The allowlist check is the **second gate** for plugins that DO
//! declare `db`.

use std::collections::HashSet;
use std::path::Path;
use std::sync::LazyLock;

use regex::Regex;
use tracing::debug;

use super::info_parser::PluginInfo;

/// Declarative error prefix for a structured call to a table outside the
/// plugin's effective allowlist (WASM-2 / D-19 §5). Frozen surface — worded once.
///
/// Follows the frozen-invoke-vocabulary style (kebab prefix, colon, detail; see
/// the error constants in `crate::host::plugin_api`). The full message is
/// `table-not-declared: <table> (plugin <name>)`. The `db` host functions cross
/// the WASM ABI as a numeric code
/// ([`trovato_sdk::host_errors::ERR_TABLE_NOT_DECLARED`]); this string is the
/// stable, human-auditable rendering logged host-side and asserted by tests.
pub const TABLE_NOT_DECLARED: &str = "table-not-declared";

/// Declarative error prefix for a raw-SQL host call (`query-raw` / `execute-raw`)
/// made without the declared `raw_sql` capability (WASM-2 / D-19 §5). Frozen
/// surface — worded once.
///
/// The full message is `raw-sql-not-declared: <plugin>`. Crosses the ABI as
/// [`trovato_sdk::host_errors::ERR_RAW_SQL_NOT_DECLARED`].
pub const RAW_SQL_NOT_DECLARED: &str = "raw-sql-not-declared";

/// Matches a `CREATE TABLE [IF NOT EXISTS] [schema.]name` header, capturing the
/// (optionally schema-qualified, optionally quoted) table identifier.
///
/// Group 1 is the first identifier (the table when unqualified, or the schema
/// when qualified); group 2, when present, is the table after a `.` qualifier.
///
/// # Panics
///
/// Panics if the hard-coded regex literal is invalid (impossible in practice).
#[allow(clippy::expect_used)]
static CREATE_TABLE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?i)\bcreate\s+table\s+(?:if\s+not\s+exists\s+)?("[^"]+"|[A-Za-z_][A-Za-z0-9_$]*)(?:\s*\.\s*("[^"]+"|[A-Za-z_][A-Za-z0-9_$]*))?"#,
    )
    .expect("valid CREATE TABLE regex literal")
});

/// Extract the table names created by a plugin migration's SQL.
///
/// Scans for `CREATE TABLE` statements — handling `IF NOT EXISTS`, schema
/// qualifiers (`schema.table` → `table`), and double-quoted identifiers
/// (`"My Table"` → `My Table`). Names are returned verbatim (no case folding);
/// all Trovato tables are lowercase snake_case by convention, and structured
/// host calls are constrained to `[A-Za-z_][A-Za-z0-9_]*` identifiers, so a
/// quoted/qualified oddity in a migration cannot be reached by a structured call
/// regardless.
pub fn extract_created_tables(sql: &str) -> Vec<String> {
    let mut tables = Vec::new();
    for caps in CREATE_TABLE_RE.captures_iter(sql) {
        // Schema-qualified: group 2 is the table; otherwise group 1 is.
        let raw = caps.get(2).or_else(|| caps.get(1));
        if let Some(m) = raw {
            let name = m.as_str().trim().trim_matches('"');
            if !name.is_empty() {
                tables.push(name.to_string());
            }
        }
    }
    tables
}

/// The database-scoping policy enforced against one plugin's `db` host calls.
///
/// Built once at load ([`DbPolicy::derive`]); cheap to clone into per-request
/// state (small `HashSet` of table names). See the module docs for the derivation
/// rule.
#[derive(Debug, Clone, Default)]
pub struct DbPolicy {
    /// Plugin machine name, used only to render error detail.
    plugin: String,
    /// Effective table allowlist: migration-owned ∪ manifest `db_tables`.
    tables: HashSet<String>,
    /// Whether the plugin declared `raw_sql = true` (gates `query-raw` /
    /// `execute-raw`).
    raw_sql: bool,
}

impl DbPolicy {
    /// Construct a policy from explicit parts (derivation and tests).
    pub(crate) fn from_parts(
        plugin: impl Into<String>,
        tables: impl IntoIterator<Item = String>,
        raw_sql: bool,
    ) -> Self {
        Self {
            plugin: plugin.into(),
            tables: tables.into_iter().collect(),
            raw_sql,
        }
    }

    /// Derive the effective policy from a plugin's manifest and migration SQL.
    ///
    /// Reads each declared migration file relative to `plugin_dir`, unions the
    /// `CREATE TABLE` names with the manifest's explicit `db_tables`, and carries
    /// the `raw_sql` flag. A missing or unreadable migration file is skipped here
    /// (the migration runner reports it at apply time — see
    /// [`crate::plugin::migration::run_plugin_migrations`]); this keeps plugin
    /// load resilient when a compiled-artifact tree lacks the migration sources.
    pub fn derive(info: &PluginInfo, plugin_dir: &Path) -> Self {
        let mut tables: HashSet<String> = HashSet::new();

        // Migration-owned tables.
        for file in &info.migrations.files {
            let path = plugin_dir.join(file);
            match std::fs::read_to_string(&path) {
                Ok(sql) => tables.extend(extract_created_tables(&sql)),
                Err(e) => debug!(
                    plugin = %info.name,
                    file = %file,
                    error = %e,
                    "migration file unreadable while deriving db allowlist; skipping",
                ),
            }
        }

        // Manifest additions.
        let raw_sql = info.capabilities.as_ref().is_some_and(|c| c.raw_sql);
        if let Some(caps) = &info.capabilities {
            tables.extend(caps.db_tables.iter().cloned());
        }

        Self::from_parts(info.name.clone(), tables, raw_sql)
    }

    /// Enforce the structured-call table allowlist.
    ///
    /// `Ok(())` if `table` is in the effective allowlist; otherwise `Err` with the
    /// frozen [`TABLE_NOT_DECLARED`] message.
    pub fn check_table(&self, table: &str) -> Result<(), String> {
        if self.tables.contains(table) {
            Ok(())
        } else {
            Err(format!(
                "{TABLE_NOT_DECLARED}: {table} (plugin {})",
                self.plugin
            ))
        }
    }

    /// Enforce the raw-SQL gate.
    ///
    /// `Ok(())` if the plugin declared `raw_sql = true`; otherwise `Err` with the
    /// frozen [`RAW_SQL_NOT_DECLARED`] message.
    pub fn check_raw_sql(&self) -> Result<(), String> {
        if self.raw_sql {
            Ok(())
        } else {
            Err(format!("{RAW_SQL_NOT_DECLARED}: {}", self.plugin))
        }
    }
}

#[cfg(test)]
// Tests are allowed to use unwrap/expect freely.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn extract_plain_create_table() {
        let sql = "CREATE TABLE importer_jobs (id bigint primary key);";
        assert_eq!(extract_created_tables(sql), vec!["importer_jobs"]);
    }

    #[test]
    fn extract_if_not_exists() {
        let sql = "create table if not exists pagefind_index_status (id int);";
        assert_eq!(extract_created_tables(sql), vec!["pagefind_index_status"]);
    }

    #[test]
    fn extract_quoted_identifier() {
        let sql = r#"CREATE TABLE "weird table" (id int);"#;
        assert_eq!(extract_created_tables(sql), vec!["weird table"]);
    }

    #[test]
    fn extract_schema_qualified_takes_table_not_schema() {
        let sql = "CREATE TABLE public.events (id int);";
        assert_eq!(extract_created_tables(sql), vec!["events"]);
    }

    #[test]
    fn extract_schema_qualified_quoted() {
        let sql = r#"CREATE TABLE "app"."Devices" (id int);"#;
        assert_eq!(extract_created_tables(sql), vec!["Devices"]);
    }

    #[test]
    fn extract_multiple_statements() {
        let sql = "
            CREATE TABLE alpha (id int);
            INSERT INTO alpha VALUES (1);
            CREATE TABLE IF NOT EXISTS beta (id int);
            CREATE TABLE gamma (id int);
        ";
        assert_eq!(extract_created_tables(sql), vec!["alpha", "beta", "gamma"]);
    }

    #[test]
    fn extract_ignores_non_create_table() {
        let sql = "CREATE INDEX idx ON alpha (id); ALTER TABLE alpha ADD COLUMN x int;";
        assert!(extract_created_tables(sql).is_empty());
    }

    #[test]
    fn check_table_allows_listed_and_rejects_others_with_exact_message() {
        let policy = DbPolicy::from_parts("importer", ["importer_jobs".to_string()], false);
        assert!(policy.check_table("importer_jobs").is_ok());
        assert_eq!(
            policy.check_table("users").unwrap_err(),
            "table-not-declared: users (plugin importer)"
        );
    }

    #[test]
    fn check_raw_sql_gate_exact_message() {
        let denied = DbPolicy::from_parts("reader", Vec::<String>::new(), false);
        assert_eq!(
            denied.check_raw_sql().unwrap_err(),
            "raw-sql-not-declared: reader"
        );
        let allowed = DbPolicy::from_parts("importer", Vec::<String>::new(), true);
        assert!(allowed.check_raw_sql().is_ok());
    }

    #[test]
    fn derive_unions_migration_tables_with_manifest_db_tables() {
        use crate::plugin::info_parser::{MigrationConfig, PluginCapabilities, TapConfig};

        let dir = std::env::temp_dir().join("trovato_db_policy_derive");
        std::fs::create_dir_all(dir.join("migrations")).unwrap();
        std::fs::write(
            dir.join("migrations/001_init.sql"),
            "CREATE TABLE owned_a (id int);\nCREATE TABLE IF NOT EXISTS owned_b (id int);",
        )
        .unwrap();

        let info = PluginInfo {
            name: "sample".to_string(),
            description: "d".to_string(),
            version: "1.0.0".to_string(),
            api_version: "0.2".to_string(),
            default_enabled: true,
            dependencies: vec![],
            taps: TapConfig::default(),
            migrations: MigrationConfig {
                files: vec!["migrations/001_init.sql".to_string()],
                depends_on: vec![],
            },
            capabilities: Some(PluginCapabilities {
                host_interfaces: vec!["db".to_string()],
                db_tables: vec!["shared_c".to_string()],
                raw_sql: true,
                ai_background: false,
                http_max_transfer: None,
                public_functions: vec![],
            }),
            record_types: vec![],
        };

        let policy = DbPolicy::derive(&info, &dir);
        // Migration-owned even though not in the manifest db_tables.
        assert!(policy.check_table("owned_a").is_ok());
        assert!(policy.check_table("owned_b").is_ok());
        // Manifest-declared, not migration-owned.
        assert!(policy.check_table("shared_c").is_ok());
        // Neither.
        assert!(policy.check_table("nope").is_err());
        // raw_sql carried through.
        assert!(policy.check_raw_sql().is_ok());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn derive_none_capabilities_denies_all() {
        use crate::plugin::info_parser::{MigrationConfig, TapConfig};

        let info = PluginInfo {
            name: "legacy".to_string(),
            description: "d".to_string(),
            version: "1.0.0".to_string(),
            api_version: "0.2".to_string(),
            default_enabled: true,
            dependencies: vec![],
            taps: TapConfig::default(),
            migrations: MigrationConfig::default(),
            capabilities: None,
            record_types: vec![],
        };
        let policy = DbPolicy::derive(&info, Path::new("/nonexistent"));
        assert!(policy.check_table("anything").is_err());
        assert!(policy.check_raw_sql().is_err());
    }
}
