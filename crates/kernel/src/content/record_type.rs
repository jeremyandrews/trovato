//! Lightweight-record type registry (P11g / D-53, D-54).
//!
//! A [`RecordTypeRegistry`] is the resolved, validated view of every
//! `[[record_types]]` declaration across all loaded plugins. It is built once at
//! startup from the plugin manifests (after the content-type registry, so record
//! names can be checked against content-type names) and consumed read-only by
//! gather, the FR-8 field-access seam, admin listing/view, and RecordReference
//! resolution.
//!
//! Manifest parse already guarantees each [`RecordTypeDecl`] is *structurally*
//! valid (machine-name, safe identifiers, resolvable field targets). This layer
//! adds the two checks that need the whole loaded system:
//!
//! - the backing table falls inside the owning plugin's **effective DB
//!   allowlist** (migration-owned union `db_tables`) — a plugin may only expose a
//!   table it is itself allowed to touch (WASM-2 / D-19 posture); and
//! - the record type **name is unique** across all record types and does not
//!   collide with a content type — record names share the `item_type` slot the
//!   FR-8 seam and gather key on.
//!
//! A declaration that fails either check is **skipped with a collected error**
//! (logged at startup) rather than aborting kernel boot: one plugin's bad record
//! declaration must not take down the site, matching how plugin load errors are
//! collected.

use std::collections::{HashMap, HashSet};

use crate::plugin::{DbPolicy, RecordTypeDecl};

/// A resolved, validated lightweight-record type.
///
/// The manifest [`RecordTypeDecl`] plus the owning plugin name, admitted to the
/// registry only after the allowlist and uniqueness checks pass. All identifier
/// fields are safe SQL identifiers (validated at manifest parse); field-map
/// targets are a plain column or a `fields.`-rooted JSONB path.
#[derive(Debug, Clone)]
pub struct RecordTypeDef {
    /// Record-type machine name (unique across record + content types).
    pub name: String,
    /// The plugin that declared this record type (provenance / admin display).
    pub plugin: String,
    /// Backing base table (inside the plugin's effective DB allowlist).
    pub table: String,
    /// Primary-key column. Any scalar type — the kernel's read surfaces compare
    /// it as text rather than assuming a uuid.
    pub id_column: String,
    /// Title/label column.
    pub title_column: String,
    /// Creation-timestamp column.
    pub created_column: String,
    /// Last-changed-timestamp column.
    pub changed_column: String,
    /// Optional author/owner column.
    pub author_column: Option<String>,
    /// Optional published-flag column. `None` ⇒ always published.
    pub published_column: Option<String>,
    /// Logical-field-name → column-or-JSONB-path mapping.
    pub field_map: HashMap<String, String>,
}

impl RecordTypeDef {
    fn from_decl(plugin: &str, decl: &RecordTypeDecl) -> Self {
        Self {
            name: decl.name.clone(),
            plugin: plugin.to_string(),
            table: decl.table.clone(),
            id_column: decl.id_column.clone(),
            title_column: decl.title_column.clone(),
            created_column: decl.created_column.clone(),
            changed_column: decl.changed_column.clone(),
            author_column: decl.author_column.clone(),
            published_column: decl.published_column.clone(),
            field_map: decl.fields.clone(),
        }
    }

    /// Resolve a logical field name to the column-or-JSONB-path a gather should
    /// query, consulting the structural columns first and then the declared field
    /// map. Returns `None` for a name that is neither structural nor mapped (the
    /// caller then leaves the reference untranslated).
    ///
    /// The structural names (`id`, `title`, `created`, `changed`, and — when
    /// declared — `author`, `published`) let a gather filter and sort on a
    /// record's item-like columns by their logical role, independent of the
    /// physical column names the plugin chose.
    pub fn resolve_field<'a>(&'a self, logical: &'a str) -> Option<&'a str> {
        match logical {
            "id" => Some(&self.id_column),
            "title" => Some(&self.title_column),
            "created" => Some(&self.created_column),
            "changed" => Some(&self.changed_column),
            "author" => self.author_column.as_deref(),
            "published" => self.published_column.as_deref(),
            other => self.field_map.get(other).map(String::as_str),
        }
    }
}

/// A record-type declaration rejected at registry build, retained for startup
/// logging (mirrors `PluginLoadError`).
#[derive(Debug, Clone)]
pub struct RecordTypeLoadError {
    /// The plugin that declared the rejected record type.
    pub plugin: String,
    /// The rejected record type name.
    pub record_type: String,
    /// Why it was rejected.
    pub reason: String,
}

impl std::fmt::Display for RecordTypeLoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "record type '{}' (plugin '{}'): {}",
            self.record_type, self.plugin, self.reason
        )
    }
}

/// The resolved lightweight-record types, keyed by record-type name.
///
/// Immutable after [`Self::build`] — record declarations come from static plugin
/// manifests, so unlike the content-type registry there is no TTL reload.
#[derive(Debug, Default)]
pub struct RecordTypeRegistry {
    types: HashMap<String, RecordTypeDef>,
}

impl RecordTypeRegistry {
    /// Build the registry from every plugin's declared record types.
    ///
    /// `plugins` yields `(plugin_name, effective_db_policy, declarations)` per
    /// loaded plugin. `content_type_names` is the set of content-type machine
    /// names a record type may not collide with. Plugins are processed in a
    /// deterministic (name-sorted) order so a duplicate record-type name resolves
    /// to a stable first-wins winner. Rejected declarations are returned as
    /// errors, not admitted.
    pub fn build<'a>(
        plugins: impl IntoIterator<Item = (&'a str, &'a DbPolicy, &'a [RecordTypeDecl])>,
        content_type_names: &HashSet<String>,
    ) -> (Self, Vec<RecordTypeLoadError>) {
        let mut sources: Vec<(&str, &DbPolicy, &[RecordTypeDecl])> = plugins.into_iter().collect();
        sources.sort_by(|a, b| a.0.cmp(b.0));

        let mut types: HashMap<String, RecordTypeDef> = HashMap::new();
        let mut errors: Vec<RecordTypeLoadError> = Vec::new();

        for (plugin_name, policy, decls) in sources {
            for decl in decls {
                if let Err(reason) =
                    Self::admit(plugin_name, policy, decl, content_type_names, &types)
                {
                    errors.push(RecordTypeLoadError {
                        plugin: plugin_name.to_string(),
                        record_type: decl.name.clone(),
                        reason,
                    });
                    continue;
                }
                types.insert(
                    decl.name.clone(),
                    RecordTypeDef::from_decl(plugin_name, decl),
                );
            }
        }

        (Self { types }, errors)
    }

    /// Validate one declaration against the effective allowlist and the name
    /// namespace. `Ok(())` ⇒ admit; `Err(reason)` ⇒ reject.
    fn admit(
        _plugin: &str,
        policy: &DbPolicy,
        decl: &RecordTypeDecl,
        content_type_names: &HashSet<String>,
        admitted: &HashMap<String, RecordTypeDef>,
    ) -> Result<(), String> {
        // Effective-allowlist cross-check (WASM-2 / D-19): a plugin may only
        // expose a table it is itself allowed to touch.
        if let Err(e) = policy.check_table(&decl.table) {
            return Err(format!(
                "backing table '{}' is not in the plugin's effective DB allowlist ({e})",
                decl.table
            ));
        }
        // Record names share the FR-8 / gather `item_type` slot with content
        // types, so they must not collide with one.
        if content_type_names.contains(&decl.name) {
            return Err(format!("name collides with content type '{}'", decl.name));
        }
        // Cross-plugin uniqueness among record types (first-wins, deterministic).
        if let Some(existing) = admitted.get(&decl.name) {
            return Err(format!(
                "name already declared by plugin '{}'",
                existing.plugin
            ));
        }
        Ok(())
    }

    /// Look up a resolved record type by name.
    pub fn get(&self, name: &str) -> Option<&RecordTypeDef> {
        self.types.get(name)
    }

    /// Whether `name` is a registered record type.
    pub fn contains(&self, name: &str) -> bool {
        self.types.contains_key(name)
    }

    /// All resolved record types, sorted by name for stable listing.
    pub fn list(&self) -> Vec<&RecordTypeDef> {
        let mut out: Vec<&RecordTypeDef> = self.types.values().collect();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }

    /// Number of registered record types.
    pub fn len(&self) -> usize {
        self.types.len()
    }

    /// Whether no record types are registered (the common case).
    pub fn is_empty(&self) -> bool {
        self.types.is_empty()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::plugin::{MigrationConfig, PluginCapabilities, PluginInfo, TapConfig};
    use std::path::Path;

    /// Build a `DbPolicy` whose effective allowlist is exactly `db_tables`
    /// (no migration-owned tables — the dir does not exist).
    fn policy_with_tables(tables: &[&str]) -> DbPolicy {
        let info = PluginInfo {
            name: "p".to_string(),
            description: "d".to_string(),
            version: "1.0.0".to_string(),
            api_version: "0.2".to_string(),
            default_enabled: true,
            dependencies: vec![],
            taps: TapConfig::default(),
            migrations: MigrationConfig::default(),
            capabilities: Some(PluginCapabilities {
                host_interfaces: vec!["db".to_string()],
                db_tables: tables.iter().map(|t| t.to_string()).collect(),
                raw_sql: false,
                ai_background: false,
                http_max_transfer: None,
                public_functions: vec![],
            }),
            record_types: vec![],
        };
        DbPolicy::derive(&info, Path::new("/nonexistent-plugin-dir"))
    }

    fn decl(name: &str, table: &str) -> RecordTypeDecl {
        RecordTypeDecl {
            name: name.to_string(),
            table: table.to_string(),
            id_column: "id".to_string(),
            title_column: "title".to_string(),
            created_column: "created".to_string(),
            changed_column: "changed".to_string(),
            author_column: None,
            published_column: None,
            fields: HashMap::new(),
        }
    }

    #[test]
    fn admits_declaration_backed_by_allowlisted_table() {
        let policy = policy_with_tables(&["conf_records"]);
        let decls = vec![decl("conference", "conf_records")];
        let (reg, errors) =
            RecordTypeRegistry::build([("events", &policy, decls.as_slice())], &HashSet::new());
        assert!(errors.is_empty(), "unexpected errors: {errors:?}");
        assert_eq!(reg.len(), 1);
        let def = reg.get("conference").unwrap();
        assert_eq!(def.table, "conf_records");
        assert_eq!(def.plugin, "events");
    }

    #[test]
    fn rejects_table_outside_allowlist() {
        let policy = policy_with_tables(&["some_other_table"]);
        let decls = vec![decl("conference", "conf_records")];
        let (reg, errors) =
            RecordTypeRegistry::build([("events", &policy, decls.as_slice())], &HashSet::new());
        assert!(reg.is_empty());
        assert_eq!(errors.len(), 1);
        assert!(errors[0].reason.contains("effective DB allowlist"));
    }

    #[test]
    fn rejects_name_colliding_with_content_type() {
        let policy = policy_with_tables(&["conf_records"]);
        let decls = vec![decl("conference", "conf_records")];
        let mut content_types = HashSet::new();
        content_types.insert("conference".to_string());
        let (reg, errors) =
            RecordTypeRegistry::build([("events", &policy, decls.as_slice())], &content_types);
        assert!(reg.is_empty());
        assert_eq!(errors.len(), 1);
        assert!(errors[0].reason.contains("content type"));
    }

    #[test]
    fn rejects_cross_plugin_duplicate_name_first_wins() {
        let policy_a = policy_with_tables(&["a_table"]);
        let policy_b = policy_with_tables(&["b_table"]);
        let decls_a = vec![decl("conference", "a_table")];
        let decls_b = vec![decl("conference", "b_table")];
        // Plugins processed name-sorted: "aaa" wins over "bbb".
        let (reg, errors) = RecordTypeRegistry::build(
            [
                ("bbb", &policy_b, decls_b.as_slice()),
                ("aaa", &policy_a, decls_a.as_slice()),
            ],
            &HashSet::new(),
        );
        assert_eq!(reg.len(), 1);
        assert_eq!(reg.get("conference").unwrap().plugin, "aaa");
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].plugin, "bbb");
        assert!(errors[0].reason.contains("already declared"));
    }

    #[test]
    fn resolve_field_covers_structural_and_mapped_names() {
        let mut d = decl("conference", "conf_records");
        d.title_column = "name".to_string();
        d.author_column = Some("owner_id".to_string());
        d.fields.insert("venue".to_string(), "location".to_string());
        d.fields
            .insert("capacity".to_string(), "fields.capacity".to_string());
        let def = RecordTypeDef::from_decl("events", &d);
        assert_eq!(def.resolve_field("title"), Some("name"));
        assert_eq!(def.resolve_field("id"), Some("id"));
        assert_eq!(def.resolve_field("author"), Some("owner_id"));
        assert_eq!(def.resolve_field("published"), None); // not declared
        assert_eq!(def.resolve_field("venue"), Some("location"));
        assert_eq!(def.resolve_field("capacity"), Some("fields.capacity"));
        assert_eq!(def.resolve_field("nonexistent"), None);
    }
}
