//! Parser for plugin `.info.toml` manifest files.
//!
//! Each plugin has a `{name}.info.toml` file that declares metadata:
//! - name, version, description
//! - dependencies (other plugins that must load first)
//! - taps (which tap functions the plugin implements)

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Plugin metadata parsed from `.info.toml`.
#[derive(Debug, Clone, Deserialize)]
pub struct PluginInfo {
    /// Plugin machine name (must match directory and file names).
    pub name: String,

    /// Human-readable description.
    pub description: String,

    /// Semantic version (e.g., "0.102.0").
    pub version: String,

    /// Plugin API version compatibility target (e.g., "0.102").
    #[serde(default = "default_api_version")]
    pub api_version: String,

    /// Whether this plugin should be auto-enabled on first install.
    #[serde(default = "default_true")]
    pub default_enabled: bool,

    /// Other plugins this one depends on (loaded first).
    #[serde(default)]
    pub dependencies: Vec<String>,

    /// Tap configuration.
    #[serde(default)]
    pub taps: TapConfig,

    /// Migration configuration.
    #[serde(default)]
    pub migrations: MigrationConfig,

    /// Declared plugin capabilities — the plugin-boundary contract surface
    /// (WASM-1 host interfaces + WASM-2 DB-table allowlist + raw-SQL gate).
    ///
    /// **Enforced (PF-2 / D-2 / D-19).** `None` when the `[capabilities]` table is
    /// absent from the manifest, which now means **deny**: the WASM-1 per-plugin
    /// linker exposes no host interface, and (WASM-2) the derived DB allowlist is
    /// empty with `raw_sql` off. See [`PluginCapabilities`] for the per-field
    /// contract.
    #[serde(default)]
    pub capabilities: Option<PluginCapabilities>,

    /// Lightweight-record type declarations (P11g / D-53).
    ///
    /// Each entry declares one of the plugin's own allowlisted tables as an
    /// **item-like** record type that the kernel then serves through gather,
    /// admin listing/view, the FR-8 field-access seam, and RecordReference
    /// resolution — **without** the full Item machinery (no revision row, no
    /// synchronous embed, no forced kernel ownership). The plugin owns writes to
    /// the table through its existing `db` capability; the kernel owns the read /
    /// gather / access surfaces. See [`RecordTypeDecl`].
    ///
    /// Declarative by design (D-53, rejecting migration-time registration): the
    /// shape is validated at manifest parse and enters the FR-4 1.0 freeze (D-59).
    /// Structural validity is checked here; the cross-check that
    /// [`RecordTypeDecl::table`] falls inside the plugin's effective DB allowlist
    /// (migration-owned ∪ `db_tables`) happens at registry build, where the
    /// derived allowlist is known.
    #[serde(default)]
    pub record_types: Vec<RecordTypeDecl>,
}

/// A lightweight-record type declaration (P11g / D-53) — the manifest surface
/// that enters the FR-4 1.0 contract freeze (D-59, FREEZE-GATING).
///
/// Declares that one plugin-owned, allowlisted table has an **item-like** shape:
/// a UUID primary key, a title column, created/changed timestamps, an optional
/// author column, an optional published flag (absent ⇒ always published), and a
/// **logical-field-name → column-or-JSONB-path** mapping consumed by gather.
///
/// The declaration is purely descriptive: it states *which columns carry which
/// item-like role*, so the kernel's already-parameterized gather `base_table`
/// (delta #2) and shape-agnostic FR-8 decision core (delta #1) can serve the
/// table with no per-plugin code. All identifier fields are validated as safe
/// SQL identifiers (or, for [`Self::fields`] values, safe dotted JSONB paths) at
/// manifest parse so they can be interpolated into SeaQuery `Alias`es without an
/// injection surface.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
pub struct RecordTypeDecl {
    /// Record-type machine name (unique across all loaded plugins). This is the
    /// type-name string the FR-8 field-access seam and gather key on, in the same
    /// slot an Item content type occupies — so it must not collide with a content
    /// type or another record type. Validated as a machine name.
    pub name: String,

    /// The plugin-owned base table this record type is backed by. Must fall
    /// inside the plugin's effective DB allowlist (migration-owned ∪ `db_tables`)
    /// — enforced at registry build, not here (the allowlist is derived at load).
    /// Validated as a safe SQL identifier here.
    pub table: String,

    /// Primary-key column, of any scalar type — the kernel's read surfaces
    /// compare it as text rather than assuming a uuid. Defaults to `"id"`.
    #[serde(default = "default_id_column")]
    pub id_column: String,

    /// Title/label column surfaced by gather and admin listing. Required.
    pub title_column: String,

    /// Creation-timestamp column. Defaults to `"created"`.
    #[serde(default = "default_created_column")]
    pub created_column: String,

    /// Last-changed-timestamp column. Defaults to `"changed"`.
    #[serde(default = "default_changed_column")]
    pub changed_column: String,

    /// Optional author/owner column (UUID). `None` ⇒ the record type has no
    /// author dimension.
    #[serde(default)]
    pub author_column: Option<String>,

    /// Optional published-flag column (boolean or `status`-style smallint).
    /// `None` ⇒ the record type is **always published** (every row visible to
    /// the record-level filter; field visibility is still governed by the FR-8
    /// seam).
    #[serde(default)]
    pub published_column: Option<String>,

    /// Logical-field-name → column-or-JSONB-path mapping consumed by gather. A
    /// value is either a plain column name (`"venue"`) or a dotted JSONB path
    /// rooted at a JSONB column (`"data.capacity"`). Keys are the logical names a
    /// gather definition references; values are validated as safe field
    /// references so gather can resolve them without an injection surface.
    #[serde(default)]
    pub fields: HashMap<String, String>,
}

/// Declared plugin capabilities — the plugin-boundary contract surface.
///
/// **Enforced contract surface (PF-2 / D-2 / D-19).** The kernel builds a
/// per-plugin linker from `host_interfaces` (WASM-1 / D-18), confines structured
/// `db` calls to the effective allowlist derived from `db_tables` + migration-owned
/// tables (WASM-2 / D-19), and gates the raw-SQL host functions on `raw_sql`
/// (WASM-2 / D-19).
///
/// # Deny-unless-declared
///
/// When the `[capabilities]` table is absent ([`PluginInfo::capabilities`] is
/// `None`), the plugin receives **no** host interface, an **empty** DB allowlist,
/// and `raw_sql` off — deny-by-default. A plugin that declares `[capabilities]`
/// gets exactly the subset it names.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct PluginCapabilities {
    /// Host interfaces this plugin may import (WASM-1 / FR-2-F3), named by their
    /// WIT interface identifier (e.g. `"item-api"`, `"db"`, `"logging"`,
    /// `"crypto-api"`). See [`KNOWN_HOST_INTERFACES`]. An empty list declares that
    /// the plugin imports no host interface.
    ///
    /// Enforced (WASM-1 / D-18): the per-plugin linker exposes only this subset.
    #[serde(default)]
    pub host_interfaces: Vec<String>,

    /// Explicit database-table allowlist (WASM-2 / FR-2-F4 / D-19). **Enforced**:
    /// structured `db` calls (select/insert/update/delete) to a table outside the
    /// effective allowlist are rejected (`table-not-declared`) before any query.
    ///
    /// The kernel's *default* allowlist is **derived from the plugin's
    /// migration-owned tables** — the `CREATE TABLE`s declared in
    /// [`MigrationConfig::files`]. This field is an optional explicit extension for
    /// tables a plugin must touch but does not own through its own migrations.
    ///
    /// Mechanism-agnostic by design (the **Option-B seam**, D-19): the declaration
    /// states *which tables the plugin touches*, not *how the limit is enforced*.
    /// The same list can drive kernel-side allowlist checks in `host/db.rs`
    /// (Option A, the 1.0 plane) or a per-plugin Postgres role `GRANT`ed on exactly
    /// these tables (Option B, post-1.0, opt-in, out-of-band) with no contract change.
    #[serde(default)]
    pub db_tables: Vec<String>,

    /// Whether this plugin may call the raw-SQL host functions `query-raw` /
    /// `execute-raw` (D-19 `execute-raw` disposition). Defaults to `false`: raw SQL
    /// is gated behind this separately-declared capability rather than dropped,
    /// because `ritrovo_importer` (the reference importer) legitimately needs it.
    ///
    /// **Declaring `raw_sql = true` weakens the table-allowlist guarantee for this
    /// plugin.** The kernel cannot reliably parse arbitrary SQL to confine it to
    /// [`Self::db_tables`] (the SQLI-1 surface), so a raw-SQL plugin can reach any
    /// table its DB role can. This is an accepted, **declared, auditable** risk —
    /// visible in the manifest and reviewable at install time.
    #[serde(default)]
    pub raw_sql: bool,

    /// Whether this plugin may call the AI host function `ai-request` from a
    /// **background** dispatch context — cron (`tap_cron`) or the queue worker
    /// (`tap_queue_worker`) — under the kernel-internal background principal
    /// (P11c / D-40, D-41). Defaults to `false`.
    ///
    /// Gated on the **existing manifest-capability plane** (the same plane as
    /// [`Self::raw_sql`]), *not* a second permission plane: the human `use ai`
    /// permission still governs every web/user AI call unchanged. A background
    /// context has no human identity to carry `use ai`, so the kernel authorizes
    /// its AI call by this declared, auditable capability instead. Without it, a
    /// plugin's `ai-request` from a background context is denied
    /// (`ERR_AI_BACKGROUND_DENIED`); with it, the call proceeds and is attributed
    /// to this plugin in `ai_usage_log` (`user_id = NULL`, `plugin_name = <this>`)
    /// and enforced against this plugin's per-plugin token budget (D-42).
    #[serde(default)]
    pub ai_background: bool,

    /// Maximum total bytes a single streaming HTTP fetch (`http-open` /
    /// `http-read`, P11e / D-50) may transfer over the wire, in bytes.
    ///
    /// **Manifest-declared, kernel-capped (D-50).** The kernel clamps the declared
    /// value to `[1, 16 MB]`; when the field is absent the ceiling is the 1 MB
    /// default (matching the `request` one-shot cap, so a plugin that never
    /// streams sees today's behavior). The clamp lives in
    /// [`CompiledPlugin::http_max_transfer`](crate::plugin::CompiledPlugin::http_max_transfer),
    /// not here, so a manifest can never grant more than the kernel maximum.
    ///
    /// This bounds only the additive streaming path. `request` is unchanged: it
    /// keeps its own fixed 1 MB response-body cap regardless of this field.
    #[serde(default)]
    pub http_max_transfer: Option<u64>,

    /// Functions this plugin exposes for invocation by other plugins (FR-4a).
    ///
    /// Names match the plugin's WASM exports. Deny-by-default: absent or empty
    /// means the plugin exposes no invocable surface. This is the plugin's
    /// public API for `plugin-api::invoke` (Story 2.2 / D-14).
    ///
    /// This is the **callee consent gate** of the FR-4a invocation model
    /// ([pure Model C], approved by Jeremy 2026-06-04): a plugin's internal
    /// functions stay private; only the functions listed here are reachable via
    /// `invoke`. Like [`Self::host_interfaces`] (where `capabilities: None` denies
    /// every host import — the WASM-1 flip, no longer grant-all), invocation has
    /// **no pre-existing consumers to preserve**, so `capabilities: None` and an
    /// absent/empty list are treated identically as *no public functions* —
    /// deny-by-default from day one. The enforcing check lives in
    /// `host/plugin_api.rs::invoke`.
    ///
    /// [pure Model C]: the per-caller restriction (a caller-side `invokes`
    /// allowlist) is deferred post-1.0 as an additive tightening; any plugin
    /// holding the `plugin-api` host capability may call any published function.
    #[serde(default)]
    pub public_functions: Vec<String>,
}

/// Configuration for plugin-declared SQL migrations.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct MigrationConfig {
    /// Ordered list of SQL migration files relative to the plugin directory.
    #[serde(default)]
    pub files: Vec<String>,

    /// Plugins whose migrations must run before this plugin's migrations.
    #[serde(default)]
    pub depends_on: Vec<String>,
}

/// Configuration for which taps a plugin implements.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct TapConfig {
    /// List of tap function names this plugin exports.
    /// E.g., ["tap_item_info", "tap_item_view", "tap_menu"]
    #[serde(default)]
    pub implements: Vec<String>,

    /// Weight for ordering (lower = higher priority, default 0).
    #[serde(default)]
    pub weight: i32,

    /// Per-tap options (reserved for future use).
    #[serde(default)]
    pub options: HashMap<String, TapOptions>,
}

/// Per-tap configuration options.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct TapOptions {
    // Reserved for future use (e.g., field filtering for filtered serialization)
}

/// Known tap names for validation.
pub const KNOWN_TAPS: &[&str] = &[
    // Lifecycle
    "tap_install",
    "tap_enable",
    "tap_disable",
    "tap_uninstall",
    // Content types
    "tap_item_info",
    // Item CRUD
    "tap_item_view",
    "tap_item_view_alter",
    "tap_item_insert",
    "tap_item_update",
    "tap_item_delete",
    "tap_item_presave",
    "tap_item_access",
    "tap_field_access",
    // Categories
    "tap_categories_term_insert",
    "tap_categories_term_update",
    "tap_categories_term_delete",
    // Forms
    "tap_form_alter",
    "tap_form_validate",
    "tap_form_submit",
    "tap_form_ajax",
    // Routing & permissions
    "tap_menu",
    // Serves one HTTP request for a `handler_type = "api"` menu entry
    // (G-NO-PLUGIN-HTTP; added in KERNEL_API_VERSION (0,99)).
    "tap_api",
    "tap_perm",
    // Theme
    "tap_theme",
    "tap_preprocess_item",
    // Search
    "tap_item_update_index",
    // Cron & queues
    "tap_cron",
    "tap_queue_info",
    "tap_queue_worker",
    // User
    "tap_user_login",
    "tap_user_logout",
    "tap_user_register",
    "tap_user_update",
    "tap_user_delete",
    "tap_user_export",
    // Account recovery (FR-7c freeze gate — Story 4.5)
    "tap_account_recovery",
    // AI governance
    "tap_ai_request",
    // Superseded by the three assistant taps below, which are dispatched.
    // `tap_chat_actions` never was; it stays declared so no manifest that names
    // it stops loading.
    "tap_chat_actions",
    // AI Assistant (added in KERNEL_API_VERSION (0,102)): a plugin declares what
    // can be configured by conversation, describes the thing being configured,
    // and answers the model's tool calls.
    "tap_assistant_scopes",
    "tap_assistant_context",
    "tap_assistant_tool",
    // NOTE: tap_csp_alter removed pre-1.0 (PF-4.1) — zero dispatch + an
    // inferred signature; re-add post-1.0 from a real CSP call site.
    // Comments
    "tap_comment_insert",
    "tap_comment_update",
    "tap_comment_delete",
    "tap_comment_access",
    // Gather extensions
    "tap_gather_extend",
];

/// Host interface names a plugin may declare under `[capabilities].host_interfaces`.
///
/// These mirror the `import`ed interfaces in the plugin `world` of
/// `crates/wit/kernel.wit` (the documentation contract). Used only to reject typo'd
/// interface names at manifest-parse time — this is manifest validation, **not**
/// capability enforcement (the per-plugin linker that actually exposes a subset is
/// Epic 2). Keep in sync with the WIT `world plugin` imports.
pub const KNOWN_HOST_INTERFACES: &[&str] = &[
    "item-api",
    "db",
    "variables",
    "request-context",
    "user-api",
    "cache-api",
    "plugin-api",
    "logging",
    "ai-api",
    "crypto-api",
    "http",
    "queue",
    "mail",
];

fn default_true() -> bool {
    true
}

fn default_api_version() -> String {
    // A manifest that omits `api_version` targets the current kernel API. This
    // string tracks KERNEL_API_VERSION and moves with the project version.
    "0.102".to_string()
}

fn default_id_column() -> String {
    "id".to_string()
}

fn default_created_column() -> String {
    "created".to_string()
}

fn default_changed_column() -> String {
    "changed".to_string()
}

/// Validate a plain SQL identifier used as a table or column name in a
/// lightweight-record declaration (P11g / D-53).
///
/// Accepts non-empty ASCII identifiers up to Postgres's 63-byte limit that start
/// with a letter or underscore and contain only letters, digits, and
/// underscores. This is the manifest-parse guard that lets the kernel interpolate
/// the name into a SeaQuery `Alias` without an injection surface — the same shape
/// as `is_safe_table_name` in the gather layer.
fn is_valid_sql_identifier(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 63
        && name.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Validate a field-map *value* in a lightweight-record declaration (P11g /
/// D-53). Two forms are accepted, matching exactly what the gather query builder
/// can resolve (`GatherQueryBuilder::field_expr`):
///
/// - a **plain column** — a single safe SQL identifier (`"venue"`), resolved to
///   `table.venue`;
/// - a **JSONB path rooted at the `fields` column** — `"fields.<key>"` or a
///   nested `"fields.<a>.<b>"`, resolved to `table.fields->>'<key>'`. The root is
///   fixed to a `fields jsonb` column, the same convention Items use, so no query
///   builder change is needed and there is no injection surface.
///
/// Any other dotted form (e.g. `"data.capacity"`) is rejected: the builder would
/// silently root it at `fields`, so accepting it into the frozen surface would
/// mean a mapping that does not do what it says. A record type that wants JSONB
/// fields declares a `fields` column, exactly like an Item.
fn is_valid_field_ref(value: &str) -> bool {
    if value.is_empty() {
        return false;
    }
    match value.split_once('.') {
        None => is_valid_sql_identifier(value),
        Some((root, rest)) => {
            root == "fields" && !rest.is_empty() && rest.split('.').all(is_valid_sql_identifier)
        }
    }
}

impl PluginInfo {
    /// Parse a plugin info file from the given path.
    pub fn parse(path: &Path) -> Result<Self> {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read plugin info file: {}", path.display()))?;

        Self::parse_str(&content, path)
    }

    /// Parse plugin info from a TOML string.
    pub fn parse_str(content: &str, path: &Path) -> Result<Self> {
        let info: PluginInfo = toml::from_str(content)
            .with_context(|| format!("failed to parse plugin info TOML at {}", path.display()))?;

        info.validate(path)?;
        Ok(info)
    }

    /// Validate the parsed plugin info.
    fn validate(&self, path: &Path) -> Result<()> {
        // Validate name is not empty
        if self.name.is_empty() {
            anyhow::bail!("plugin info at {} has empty 'name' field", path.display());
        }

        // Validate version is not empty
        if self.version.is_empty() {
            anyhow::bail!(
                "plugin '{}' at {} has empty 'version' field",
                self.name,
                path.display()
            );
        }

        // Validate api_version format (MAJOR.MINOR, both numeric)
        let api_parts: Vec<&str> = self.api_version.split('.').collect();
        if api_parts.len() != 2 || api_parts.iter().any(|p| p.parse::<u32>().is_err()) {
            anyhow::bail!(
                "plugin '{}' at {} has invalid 'api_version' field '{}' (expected MAJOR.MINOR, e.g., '0.2')",
                self.name,
                path.display(),
                self.api_version
            );
        }

        // Validate tap names are known
        for tap in &self.taps.implements {
            if !KNOWN_TAPS.contains(&tap.as_str()) {
                anyhow::bail!(
                    "plugin '{}' declares unknown tap '{}'. Known taps: {}",
                    self.name,
                    tap,
                    KNOWN_TAPS.join(", ")
                );
            }
        }

        // Validate declared host-interface names are known (manifest hygiene only —
        // not capability enforcement, which is Epic 2). Mirrors the KNOWN_TAPS check.
        if let Some(caps) = &self.capabilities {
            for iface in &caps.host_interfaces {
                if !KNOWN_HOST_INTERFACES.contains(&iface.as_str()) {
                    anyhow::bail!(
                        "plugin '{}' declares unknown host interface '{}'. Known interfaces: {}",
                        self.name,
                        iface,
                        KNOWN_HOST_INTERFACES.join(", ")
                    );
                }
            }
        }

        // Validate migration file paths: must be relative, no traversal, .sql only
        for file in &self.migrations.files {
            let p = Path::new(file);
            if p.is_absolute() {
                anyhow::bail!(
                    "plugin '{}': migration file '{}' must be a relative path",
                    self.name,
                    file
                );
            }
            if p.components()
                .any(|c| matches!(c, std::path::Component::ParentDir))
            {
                anyhow::bail!(
                    "plugin '{}': migration file '{}' contains '..' path segment",
                    self.name,
                    file
                );
            }
            if !file.ends_with(".sql") {
                anyhow::bail!(
                    "plugin '{}': migration file '{}' must have .sql extension",
                    self.name,
                    file
                );
            }
        }

        // Validate lightweight-record declarations (P11g / D-53). Structural only:
        // machine-name for the type, safe SQL identifiers for the table and every
        // column, safe field references for the map values. The allowlist
        // cross-check (table ∈ effective DB allowlist) and cross-plugin name
        // uniqueness are enforced at registry build, where the derived allowlist
        // and the full set of loaded plugins are known.
        let mut seen_names: std::collections::HashSet<&str> = std::collections::HashSet::new();
        for rt in &self.record_types {
            if !crate::routes::helpers::is_valid_machine_name(&rt.name) {
                anyhow::bail!(
                    "plugin '{}': record type name '{}' is not a valid machine name \
                     (lowercase letter first, then lowercase letters, digits, underscores)",
                    self.name,
                    rt.name
                );
            }
            if !seen_names.insert(rt.name.as_str()) {
                anyhow::bail!(
                    "plugin '{}': record type '{}' is declared more than once",
                    self.name,
                    rt.name
                );
            }
            // Every structural column (table, id, title, timestamps, and the
            // optional author/published) must be a safe SQL identifier.
            let mut columns: Vec<(&str, &str)> = vec![
                ("table", rt.table.as_str()),
                ("id_column", rt.id_column.as_str()),
                ("title_column", rt.title_column.as_str()),
                ("created_column", rt.created_column.as_str()),
                ("changed_column", rt.changed_column.as_str()),
            ];
            if let Some(author) = &rt.author_column {
                columns.push(("author_column", author.as_str()));
            }
            if let Some(published) = &rt.published_column {
                columns.push(("published_column", published.as_str()));
            }
            for (field, value) in columns {
                if !is_valid_sql_identifier(value) {
                    anyhow::bail!(
                        "plugin '{}': record type '{}' has an invalid {} '{}' \
                         (must be a safe SQL identifier: letter/underscore first, \
                         then letters, digits, underscores; ≤63 bytes)",
                        self.name,
                        rt.name,
                        field,
                        value
                    );
                }
            }
            // Field-map keys are logical names (machine names); values are safe
            // column-or-JSONB-path references.
            for (logical, target) in &rt.fields {
                if !crate::routes::helpers::is_valid_machine_name(logical) {
                    anyhow::bail!(
                        "plugin '{}': record type '{}' maps invalid logical field name '{}' \
                         (must be a valid machine name)",
                        self.name,
                        rt.name,
                        logical
                    );
                }
                if !is_valid_field_ref(target) {
                    anyhow::bail!(
                        "plugin '{}': record type '{}' maps field '{}' to invalid target '{}' \
                         (must be a column name or dotted JSONB path of safe identifiers)",
                        self.name,
                        rt.name,
                        logical,
                        target
                    );
                }
            }
        }

        Ok(())
    }

    /// Check if this plugin's declared API version is compatible with the kernel.
    ///
    /// Rule: plugin MAJOR == kernel MAJOR AND plugin MINOR <= kernel MINOR.
    /// A plugin built for API 0.102 works on a kernel serving API 0.102. A plugin
    /// built for a future minor does NOT work on an older kernel, because the
    /// host functions it expects may not exist. A plugin built for a different
    /// major does not work at all (see [`super::KERNEL_API_VERSION`]).
    pub fn check_api_compatibility(&self) -> Result<()> {
        use super::KERNEL_API_VERSION;

        let parts: Vec<u32> = self
            .api_version
            .split('.')
            .filter_map(|p| p.parse().ok())
            .collect();

        if parts.len() != 2 {
            anyhow::bail!(
                "plugin '{}' has invalid api_version '{}'",
                self.name,
                self.api_version
            );
        }

        let (plugin_major, plugin_minor) = (parts[0], parts[1]);
        let (kernel_major, kernel_minor) = KERNEL_API_VERSION;

        if plugin_major != kernel_major {
            anyhow::bail!(
                "plugin '{}' requires API {}.{} but kernel provides API {}.{}. \
                 Major version mismatch — plugin is incompatible with this kernel.",
                self.name,
                plugin_major,
                plugin_minor,
                kernel_major,
                kernel_minor
            );
        }

        if plugin_minor > kernel_minor {
            anyhow::bail!(
                "plugin '{}' requires API {}.{} but kernel provides API {}.{}. \
                 Plugin requires a newer kernel (API {}.{}+).",
                self.name,
                plugin_major,
                plugin_minor,
                kernel_major,
                kernel_minor,
                plugin_major,
                plugin_minor
            );
        }

        Ok(())
    }
}

#[cfg(test)]
// Tests are allowed to use unwrap/expect freely.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn parse_valid_info() {
        let toml = r#"
name = "trovato_blog"
description = "Provides a blog content type"
version = "1.0.0"
dependencies = ["item", "categories"]

[taps]
implements = ["tap_item_info", "tap_item_view", "tap_menu", "tap_perm"]
weight = 0
"#;

        let info = PluginInfo::parse_str(toml, Path::new("test.toml")).unwrap();
        assert_eq!(info.name, "trovato_blog");
        assert_eq!(info.version, "1.0.0");
        assert_eq!(info.dependencies, vec!["item", "categories"]);
        assert_eq!(info.taps.implements.len(), 4);
        assert_eq!(info.taps.weight, 0);
    }

    #[test]
    fn parse_minimal_info() {
        let toml = r#"
name = "minimal"
description = "A minimal plugin"
version = "0.1.0"
"#;

        let info = PluginInfo::parse_str(toml, Path::new("test.toml")).unwrap();
        assert_eq!(info.name, "minimal");
        assert!(info.dependencies.is_empty());
        assert!(info.taps.implements.is_empty());
        assert_eq!(info.taps.weight, 0);
    }

    #[test]
    fn parse_record_type_declaration() {
        let toml = r#"
name = "trovato_events"
description = "Lightweight conference records"
version = "1.0.0"

[[record_types]]
name = "conference"
table = "conf_records"
title_column = "name"
author_column = "owner_id"
published_column = "is_public"

[record_types.fields]
venue = "location"
capacity = "fields.capacity"
"#;

        let info = PluginInfo::parse_str(toml, Path::new("test.toml")).unwrap();
        assert_eq!(info.record_types.len(), 1);
        let rt = &info.record_types[0];
        assert_eq!(rt.name, "conference");
        assert_eq!(rt.table, "conf_records");
        // Defaults applied for the columns the manifest omitted.
        assert_eq!(rt.id_column, "id");
        assert_eq!(rt.created_column, "created");
        assert_eq!(rt.changed_column, "changed");
        assert_eq!(rt.title_column, "name");
        assert_eq!(rt.author_column.as_deref(), Some("owner_id"));
        assert_eq!(rt.published_column.as_deref(), Some("is_public"));
        assert_eq!(rt.fields.get("venue").map(String::as_str), Some("location"));
        assert_eq!(
            rt.fields.get("capacity").map(String::as_str),
            Some("fields.capacity")
        );
    }

    #[test]
    fn reject_record_type_nonfields_jsonb_root() {
        // A dotted target whose root is not `fields` is rejected — the builder
        // would silently root it at the `fields` column, so it must not enter the
        // frozen surface.
        let toml = r#"
name = "trovato_events"
description = "Mistaken JSONB root"
version = "1.0.0"

[[record_types]]
name = "conference"
table = "conf_records"
title_column = "name"

[record_types.fields]
capacity = "data.capacity"
"#;

        let err = PluginInfo::parse_str(toml, Path::new("test.toml")).unwrap_err();
        assert!(err.to_string().contains("invalid target"));
    }

    #[test]
    fn parse_record_type_minimal_always_published() {
        let toml = r#"
name = "trovato_events"
description = "Minimal record type"
version = "1.0.0"

[[record_types]]
name = "note"
table = "notes"
title_column = "subject"
"#;

        let info = PluginInfo::parse_str(toml, Path::new("test.toml")).unwrap();
        let rt = &info.record_types[0];
        // No author dimension, always published (field visibility still via FR-8).
        assert!(rt.author_column.is_none());
        assert!(rt.published_column.is_none());
        assert!(rt.fields.is_empty());
    }

    #[test]
    fn reject_record_type_invalid_name() {
        let toml = r#"
name = "trovato_events"
description = "Bad record type name"
version = "1.0.0"

[[record_types]]
name = "Conference"
table = "conf_records"
title_column = "name"
"#;

        let err = PluginInfo::parse_str(toml, Path::new("test.toml")).unwrap_err();
        assert!(err.to_string().contains("valid machine name"));
    }

    #[test]
    fn reject_record_type_injection_in_column() {
        let toml = r#"
name = "trovato_events"
description = "Injection attempt in a column name"
version = "1.0.0"

[[record_types]]
name = "conference"
table = "conf_records"
title_column = "name; DROP TABLE item"
"#;

        let err = PluginInfo::parse_str(toml, Path::new("test.toml")).unwrap_err();
        assert!(err.to_string().contains("invalid title_column"));
    }

    #[test]
    fn reject_record_type_injection_in_field_target() {
        let toml = r#"
name = "trovato_events"
description = "Injection attempt in a mapped field target"
version = "1.0.0"

[[record_types]]
name = "conference"
table = "conf_records"
title_column = "name"

[record_types.fields]
venue = "location); DROP TABLE item; --"
"#;

        let err = PluginInfo::parse_str(toml, Path::new("test.toml")).unwrap_err();
        assert!(err.to_string().contains("invalid target"));
    }

    #[test]
    fn reject_duplicate_record_type_name_in_manifest() {
        let toml = r#"
name = "trovato_events"
description = "Duplicate record type name"
version = "1.0.0"

[[record_types]]
name = "conference"
table = "conf_records"
title_column = "name"

[[record_types]]
name = "conference"
table = "other_records"
title_column = "label"
"#;

        let err = PluginInfo::parse_str(toml, Path::new("test.toml")).unwrap_err();
        assert!(err.to_string().contains("declared more than once"));
    }

    #[test]
    fn reject_unknown_tap() {
        let toml = r#"
name = "bad"
description = "Bad plugin"
version = "1.0.0"

[taps]
implements = ["tap_unknown_function"]
"#;

        let result = PluginInfo::parse_str(toml, Path::new("test.toml"));
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("unknown tap"));
    }

    #[test]
    fn reject_empty_name() {
        let toml = r#"
name = ""
description = "Empty name"
version = "1.0.0"
"#;

        let result = PluginInfo::parse_str(toml, Path::new("test.toml"));
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("empty 'name'"));
    }

    #[test]
    fn reject_empty_version() {
        let toml = r#"
name = "test"
description = "Empty version"
version = ""
"#;

        let result = PluginInfo::parse_str(toml, Path::new("test.toml"));
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("empty 'version'"));
    }

    #[test]
    fn parse_migration_config() {
        let toml = r#"
name = "sample_plugin"
description = "A plugin that ships migrations"
version = "1.0.0"

[migrations]
files = ["migrations/001_create_devices.sql", "migrations/002_create_events.sql"]
depends_on = ["trovato_blog"]
"#;

        let info = PluginInfo::parse_str(toml, Path::new("test.toml")).unwrap();
        assert_eq!(info.migrations.files.len(), 2);
        assert_eq!(
            info.migrations.files[0],
            "migrations/001_create_devices.sql"
        );
        assert_eq!(info.migrations.depends_on, vec!["trovato_blog"]);
    }

    #[test]
    fn parse_no_migrations_defaults_empty() {
        let toml = r#"
name = "simple"
description = "No migrations"
version = "1.0.0"
"#;

        let info = PluginInfo::parse_str(toml, Path::new("test.toml")).unwrap();
        assert!(info.migrations.files.is_empty());
        assert!(info.migrations.depends_on.is_empty());
    }

    #[test]
    fn reject_migration_path_traversal() {
        let toml = r#"
name = "evil"
description = "Path traversal"
version = "1.0.0"

[migrations]
files = ["../../../etc/passwd.sql"]
"#;

        let result = PluginInfo::parse_str(toml, Path::new("test.toml"));
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains(".."));
    }

    #[test]
    fn reject_migration_absolute_path() {
        let toml = r#"
name = "evil"
description = "Absolute path"
version = "1.0.0"

[migrations]
files = ["/tmp/malicious.sql"]
"#;

        let result = PluginInfo::parse_str(toml, Path::new("test.toml"));
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("relative path"));
    }

    #[test]
    fn reject_migration_non_sql() {
        let toml = r#"
name = "bad"
description = "Non-SQL migration"
version = "1.0.0"

[migrations]
files = ["migrations/001_create.txt"]
"#;

        let result = PluginInfo::parse_str(toml, Path::new("test.toml"));
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains(".sql"));
    }

    #[test]
    fn parse_default_enabled_false() {
        let toml = r#"
name = "argus"
description = "News intelligence"
version = "1.0.0"
default_enabled = false
"#;

        let info = PluginInfo::parse_str(toml, Path::new("test.toml")).unwrap();
        assert!(!info.default_enabled);
    }

    #[test]
    fn default_enabled_is_true_when_omitted() {
        let toml = r#"
name = "trovato_blog"
description = "Blog plugin"
version = "1.0.0"
"#;

        let info = PluginInfo::parse_str(toml, Path::new("test.toml")).unwrap();
        assert!(info.default_enabled);
    }

    #[test]
    fn default_api_version_is_the_current_kernel_api() {
        let toml = r#"
name = "test_plugin"
description = "test"
version = "0.102.0"
"#;
        let info: PluginInfo = toml::from_str(toml).unwrap();
        let (major, minor) = super::super::KERNEL_API_VERSION;
        assert_eq!(info.api_version, format!("{major}.{minor}"));
    }

    #[test]
    fn explicit_api_version_parses() {
        let toml = r#"
name = "test_plugin"
description = "test"
version = "0.102.0"
api_version = "0.102"
"#;
        let info: PluginInfo = toml::from_str(toml).unwrap();
        assert_eq!(info.api_version, "0.102");
    }

    #[test]
    fn invalid_api_version_rejected() {
        let toml = r#"
name = "test_plugin"
description = "test"
version = "1.0.0"
api_version = "abc"
"#;
        let info: PluginInfo = toml::from_str(toml).unwrap();
        let result = info.validate(std::path::Path::new("/test"));
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("invalid 'api_version'")
        );
    }

    #[test]
    fn three_part_api_version_rejected() {
        let toml = r#"
name = "test_plugin"
description = "test"
version = "1.0.0"
api_version = "1.2.3"
"#;
        let info: PluginInfo = toml::from_str(toml).unwrap();
        let result = info.validate(std::path::Path::new("/test"));
        assert!(result.is_err());
    }

    #[test]
    fn api_compat_same_version_ok() {
        let info = make_info("0.102");
        assert!(info.check_api_compatibility().is_ok());
    }

    #[test]
    fn api_compat_older_minor_accepted() {
        // Same major, lower minor: accepted, because the rule is a compatibility
        // gate (does this kernel provide everything the plugin asks for?) and a
        // kernel at 0.102 provides everything a 0.2-era manifest declared. It is
        // NOT a provenance check; see the KERNEL_API_VERSION docs. Nothing was
        // ever released against the pre-freeze API, so no such plugin exists.
        let info = make_info("0.2");
        assert!(info.check_api_compatibility().is_ok());
    }

    #[test]
    fn api_compat_newer_minor_rejected() {
        // A future minor requires a newer kernel: it may call host functions
        // this kernel does not export.
        let info = make_info("0.103");
        let err = info.check_api_compatibility().unwrap_err();
        assert!(err.to_string().contains("requires a newer kernel"));
    }

    #[test]
    fn api_compat_major_mismatch_rejected() {
        // A different major is incompatible in either direction. Once the
        // project reaches 1.0 this is the case that keeps a 1.x plugin off a
        // 0.102 kernel.
        let info = make_info("1.0");
        let err = info.check_api_compatibility().unwrap_err();
        assert!(err.to_string().contains("Major version mismatch"));

        let info = make_info("2.0");
        let err = info.check_api_compatibility().unwrap_err();
        assert!(err.to_string().contains("Major version mismatch"));
    }

    fn make_info(api_version: &str) -> PluginInfo {
        PluginInfo {
            name: "test_plugin".to_string(),
            description: "test".to_string(),
            version: "1.0.0".to_string(),
            api_version: api_version.to_string(),
            default_enabled: true,
            dependencies: vec![],
            taps: super::TapConfig::default(),
            migrations: super::MigrationConfig::default(),
            capabilities: None,
            record_types: vec![],
        }
    }

    #[test]
    fn parse_no_capabilities_defaults_none() {
        // A manifest with no [capabilities] table parses with capabilities ==
        // None, which is deny-all under the WASM-1 flip (no host interface
        // exposed); see the PluginInfo::capabilities and build_plugin_linker docs.
        let toml = r#"
name = "legacy"
description = "No capabilities declared"
version = "1.0.0"
"#;

        let info = PluginInfo::parse_str(toml, Path::new("test.toml")).unwrap();
        assert!(info.capabilities.is_none());
    }

    #[test]
    fn parse_capabilities_full() {
        let toml = r#"
name = "ritrovo_importer"
description = "Reference importer"
version = "1.0.0"

[capabilities]
host_interfaces = ["db", "logging", "item-api"]
db_tables = ["importer_jobs", "importer_state"]
raw_sql = true
"#;

        let info = PluginInfo::parse_str(toml, Path::new("test.toml")).unwrap();
        let caps = info.capabilities.expect("capabilities present");
        assert_eq!(caps.host_interfaces, vec!["db", "logging", "item-api"]);
        assert_eq!(caps.db_tables, vec!["importer_jobs", "importer_state"]);
        assert!(caps.raw_sql);
    }

    #[test]
    fn parse_capabilities_partial_defaults() {
        // An empty [capabilities] table is distinct from an absent one:
        // Some(default) — declared, but granting nothing and raw_sql off.
        let toml = r#"
name = "locked_down"
description = "Declares capabilities but requests nothing"
version = "1.0.0"

[capabilities]
"#;

        let info = PluginInfo::parse_str(toml, Path::new("test.toml")).unwrap();
        let caps = info
            .capabilities
            .expect("empty table still deserializes to Some");
        assert!(caps.host_interfaces.is_empty());
        assert!(caps.db_tables.is_empty());
        assert!(!caps.raw_sql);
    }

    #[test]
    fn http_max_transfer_parses_and_defaults_none() {
        // Declared value round-trips as-declared (the kernel clamp lives in
        // CompiledPlugin::http_max_transfer, not the parser); absent → None.
        let declared = r#"
name = "streamer"
description = "Declares a streaming ceiling"
version = "1.0.0"

[capabilities]
host_interfaces = ["http"]
http_max_transfer = 4194304
"#;
        let info = PluginInfo::parse_str(declared, Path::new("test.toml")).unwrap();
        let caps = info.capabilities.expect("capabilities present");
        assert_eq!(caps.http_max_transfer, Some(4_194_304));

        let omitted = r#"
name = "plain"
description = "No streaming ceiling declared"
version = "1.0.0"

[capabilities]
host_interfaces = ["http"]
"#;
        let info = PluginInfo::parse_str(omitted, Path::new("test.toml")).unwrap();
        let caps = info.capabilities.expect("capabilities present");
        assert_eq!(caps.http_max_transfer, None);
    }

    #[test]
    fn raw_sql_defaults_false_when_omitted() {
        let toml = r#"
name = "reader"
description = "Structured DB only"
version = "1.0.0"

[capabilities]
host_interfaces = ["db"]
"#;

        let info = PluginInfo::parse_str(toml, Path::new("test.toml")).unwrap();
        let caps = info.capabilities.expect("capabilities present");
        assert!(!caps.raw_sql);
        assert!(caps.db_tables.is_empty());
    }

    #[test]
    fn reject_unknown_host_interface() {
        let toml = r#"
name = "typo"
description = "Misspelled interface"
version = "1.0.0"

[capabilities]
host_interfaces = ["db", "not-a-real-interface"]
"#;

        let result = PluginInfo::parse_str(toml, Path::new("test.toml"));
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("unknown host interface")
        );
    }

    #[test]
    fn all_known_host_interfaces_accepted() {
        // Every name in KNOWN_HOST_INTERFACES must validate (guards the WIT-sync list).
        let list = KNOWN_HOST_INTERFACES
            .iter()
            .map(|i| format!("\"{i}\""))
            .collect::<Vec<_>>()
            .join(", ");
        let toml = format!(
            r#"
name = "everything"
description = "Imports all host interfaces"
version = "1.0.0"

[capabilities]
host_interfaces = [{list}]
"#
        );

        let info = PluginInfo::parse_str(&toml, Path::new("test.toml")).unwrap();
        let caps = info.capabilities.expect("capabilities present");
        assert_eq!(caps.host_interfaces.len(), KNOWN_HOST_INTERFACES.len());
    }

    #[test]
    fn capabilities_round_trip_through_serde() {
        // Round-trip the parsed manifest through a serde value to confirm the
        // capabilities shape survives serialization symmetrically.
        let toml = r#"
name = "rt"
description = "round trip"
version = "1.0.0"

[capabilities]
host_interfaces = ["db", "crypto-api"]
db_tables = ["rt_table"]
raw_sql = false
"#;

        let info = PluginInfo::parse_str(toml, Path::new("test.toml")).unwrap();
        let caps = info.capabilities.as_ref().expect("capabilities present");
        let json = serde_json::to_string(caps).expect("serialize");
        let back: serde_json::Value = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back["host_interfaces"][0], "db");
        assert_eq!(back["host_interfaces"][1], "crypto-api");
        assert_eq!(back["db_tables"][0], "rt_table");
        assert_eq!(back["raw_sql"], false);
    }

    #[test]
    fn parse_public_functions_present() {
        // FR-4a callee consent gate: a plugin lists its invocable exports.
        let toml = r#"
name = "ritrovo_notify"
description = "Notification dispatcher"
version = "1.0.0"

[capabilities]
host_interfaces = ["db", "logging"]
public_functions = ["enqueue_digest", "render_badge"]
"#;

        let info = PluginInfo::parse_str(toml, Path::new("test.toml")).unwrap();
        let caps = info.capabilities.expect("capabilities present");
        assert_eq!(
            caps.public_functions,
            vec!["enqueue_digest", "render_badge"]
        );
    }

    #[test]
    fn public_functions_defaults_empty_when_omitted() {
        // Deny-by-default: a [capabilities] table without public_functions exposes
        // no invocable surface.
        let toml = r#"
name = "private"
description = "Declares capabilities but publishes no functions"
version = "1.0.0"

[capabilities]
host_interfaces = ["db"]
"#;

        let info = PluginInfo::parse_str(toml, Path::new("test.toml")).unwrap();
        let caps = info.capabilities.expect("capabilities present");
        assert!(caps.public_functions.is_empty());
    }

    #[test]
    fn public_functions_absent_when_no_capabilities_table() {
        // capabilities: None ⇒ no invocable surface (treated as deny for invoke).
        let toml = r#"
name = "legacy"
description = "No capabilities declared"
version = "1.0.0"
"#;

        let info = PluginInfo::parse_str(toml, Path::new("test.toml")).unwrap();
        assert!(info.capabilities.is_none());
    }

    #[test]
    fn public_functions_round_trip_through_serde() {
        let toml = r#"
name = "rt"
description = "round trip"
version = "1.0.0"

[capabilities]
public_functions = ["alpha", "beta"]
"#;

        let info = PluginInfo::parse_str(toml, Path::new("test.toml")).unwrap();
        let caps = info.capabilities.as_ref().expect("capabilities present");
        let json = serde_json::to_string(caps).expect("serialize");
        let back: serde_json::Value = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back["public_functions"][0], "alpha");
        assert_eq!(back["public_functions"][1], "beta");
    }
}
