//! YAML-based config export/import.
//!
//! Exports all config entities to individual YAML files (one per entity)
//! and re-imports them. File naming: `{entity_type}.{id}.yml`.
//!
//! Import is idempotent: `ConfigStorage::save()` performs upsert, so
//! re-running import on a partially-imported database converges to the
//! correct state.
//!
//! # Import is all-or-nothing on validation
//!
//! Import validates the whole config set — every file parsed, schema checked,
//! and its references resolved — before it writes anything. If any file in the
//! set fails, [`import_config`] returns [`ConfigImportFailed`] naming every
//! offending file and no write happens at all. A config set is therefore atomic
//! with respect to bad input, and a typo cannot produce a quietly partial
//! import.
//!
//! # Transaction Safety
//!
//! Once validation passes, entities are saved individually rather than in a
//! single database transaction, because [`ConfigStorage`] is a trait that may
//! wrap different backends. A save that fails at that point is still reported
//! as a failure (so the caller exits non-zero), but earlier writes stand; the
//! fix is to re-run the import, which converges because save is an upsert.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use tracing::{debug, info};
use uuid::Uuid;

use super::{ConfigEntity, ConfigStorage, SearchFieldConfig, entity_types};
use crate::gather::types::GatherQuery;
use crate::models::tile::Tile;
use crate::models::{Category, ItemType, Language, MenuLink, Role, Stage, Tag, UrlAlias};

/// Entity type ordering used for both validation and dependency-ordered import.
///
/// A single source of truth: earlier entries are imported first.
/// Categories before tags (FK), item_types before search_field_configs (bundle ref).
const ENTITY_TYPE_ORDER: &[&str] = &[
    entity_types::VARIABLE,
    entity_types::LANGUAGE,
    entity_types::ROLE,
    entity_types::ITEM_TYPE,
    entity_types::CATEGORY,
    entity_types::TAG,
    entity_types::SEARCH_FIELD_CONFIG,
    entity_types::GATHER_QUERY,
    entity_types::STAGE,
    entity_types::URL_ALIAS,
    entity_types::ITEM,
    entity_types::TILE,
    entity_types::MENU_LINK,
];

/// Maximum config file size (10 MB). Files exceeding this are skipped during import
/// to prevent unbounded memory allocation from malicious or accidental large files.
const MAX_CONFIG_FILE_SIZE: u64 = 10 * 1024 * 1024;

/// Characters that are invalid in filenames on Windows/NTFS.
/// Rejected by [`validate_entity_id_for_filename`] for cross-platform portability.
const WINDOWS_INVALID_CHARS: &[char] = &[':', '*', '?', '"', '<', '>', '|'];

/// Role with its permissions, for export/import.
///
/// The `role` config entity is the `Role` row, which does not carry permissions:
/// they are `role_permissions` rows. That made `config import` able to create a
/// role and unable to grant it anything, so a role arrived able to do nothing and
/// the tutorial's role files listed their intended permissions in comments.
///
/// `permissions` is an `Option` on purpose, and the distinction matters:
///
/// - **absent** — the file says nothing about permissions, so the role's existing
///   grants are left alone. Every role file written before this existed is in this
///   case, and treating it as "revoke everything" would mean an import silently
///   stripping a site's permissions.
/// - **present, including `[]`** — the file is authoritative and the role ends up
///   holding exactly that set.
///
/// Export always writes the key, so an exported role round-trips authoritatively.
#[derive(Serialize, Deserialize)]
struct RoleExport {
    #[serde(flatten)]
    role: Role,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    permissions: Option<Vec<String>>,
}

/// Tag with hierarchy parents for export/import.
#[derive(Serialize, Deserialize)]
struct TagExport {
    #[serde(flatten)]
    tag: Tag,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    parents: Vec<Uuid>,
}

/// Gather query YAML representation.
///
/// The `definition` and `display` fields are stored as opaque JSON values
/// rather than the typed structs. This avoids YAML tag issues with complex
/// serde enum types (e.g., `ContextualValue::UrlArg` serializes as a YAML
/// `!url_arg` tag that doesn't round-trip through standard YAML parsers).
#[derive(Serialize, Deserialize)]
struct GatherQueryExport {
    query_id: String,
    label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    definition: serde_json::Value,
    display: serde_json::Value,
    plugin: String,
}

/// Variable YAML representation (shared between export and import).
///
/// The `key` field is intentionally stored in both the filename and the file
/// content. This redundancy enables filename-content ID consistency checks
/// on import (`read_and_validate_files` warns if they disagree).
#[derive(Serialize, Deserialize)]
struct VarYaml {
    key: String,
    value: serde_json::Value,
}

/// Result summary for config export/import operations.
#[derive(Debug, Default)]
pub struct ConfigOpResult {
    pub counts: BTreeMap<String, usize>,
    pub warnings: Vec<String>,
}

impl ConfigOpResult {
    pub fn total(&self) -> usize {
        self.counts.values().sum()
    }
}

/// One config file that could not be applied, and why.
#[derive(Debug, Clone)]
pub struct ConfigImportFailure {
    /// The config file the failure belongs to.
    pub filename: String,
    /// What went wrong, with enough detail to fix the file.
    pub error: String,
}

/// Every failure from a single [`import_config`] run.
///
/// This is the error type of a failed import, deliberately: a failure that is
/// only a warning in a success result is a failure an operator can miss, and
/// for entity types whose sole management path is `config import` — roles and
/// stages — missing it means the entity silently never arrives. Returning an
/// error makes the CLI exit non-zero, and `Display` lists every offending file
/// rather than only the first.
#[derive(Debug)]
pub struct ConfigImportFailed {
    /// Every file that failed, in the order they were validated or saved.
    pub failures: Vec<ConfigImportFailure>,
    /// What was written before the failure was reported. Empty when validation
    /// failed, because validation completes before the first write.
    pub imported: BTreeMap<String, usize>,
}

impl ConfigImportFailed {
    /// Total number of entities written before the failure was reported.
    pub fn imported_total(&self) -> usize {
        self.imported.values().sum()
    }
}

impl std::fmt::Display for ConfigImportFailed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let n = self.failures.len();
        let written = self.imported_total();
        if written == 0 {
            write!(
                f,
                "config import failed: {n} file(s) did not validate, nothing was written"
            )?;
        } else {
            write!(
                f,
                "config import failed: {n} file(s) could not be saved; {written} entities were \
                 written before that and remain — re-run the import once the files below are fixed"
            )?;
        }
        for failure in &self.failures {
            write!(f, "\n  {}: {}", failure.filename, failure.error)?;
        }
        Ok(())
    }
}

impl std::error::Error for ConfigImportFailed {}

/// Generate the filename for a config entity.
fn entity_filename(entity_type: &str, id: &str) -> String {
    format!("{entity_type}.{id}.yml")
}

/// Validate that an entity ID is safe for use in a filename.
///
/// Rejects IDs containing path separators, parent-directory references,
/// null bytes, or characters invalid on Windows/NTFS — ensuring exported
/// files are portable and version-control friendly.
fn validate_entity_id_for_filename(id: &str) -> Result<()> {
    if id.is_empty() {
        anyhow::bail!("entity ID is empty");
    }
    if id.contains('/') || id.contains('\\') || id.contains('\0') {
        anyhow::bail!("entity ID contains path separator or null byte: {id}");
    }
    if id.contains("..") {
        anyhow::bail!("entity ID contains '..': {id}");
    }
    if let Some(c) = id.chars().find(|c| WINDOWS_INVALID_CHARS.contains(c)) {
        anyhow::bail!("entity ID contains character '{c}' invalid on Windows: {id}");
    }
    if id.starts_with('.') || id.ends_with('.') {
        anyhow::bail!("entity ID must not start or end with '.': {id}");
    }
    if id != id.trim() {
        anyhow::bail!("entity ID has leading/trailing whitespace: {id}");
    }
    Ok(())
}

/// Parse entity type and ID from a config filename.
///
/// Returns `None` if the filename doesn't match the expected pattern
/// or the entity type prefix is unrecognized.
fn parse_config_filename(filename: &str) -> Option<(&str, &str)> {
    let stem = filename
        .strip_suffix(".yml")
        .or_else(|| filename.strip_suffix(".yaml"))?;

    // Find the first dot to split entity_type from id.
    let dot_pos = stem.find('.')?;
    let entity_type = &stem[..dot_pos];
    let id = &stem[dot_pos + 1..];

    if id.is_empty() {
        return None;
    }

    if !ENTITY_TYPE_ORDER.contains(&entity_type) {
        return None;
    }

    Some((entity_type, id))
}

/// Serialize a config entity to YAML. Returns `None` and records a warning on failure.
fn serialize_entity(entity: &ConfigEntity, warnings: &mut Vec<String>) -> Option<String> {
    let id = entity.id();
    let result = match entity {
        ConfigEntity::ItemType(it) => serde_yml::to_string(it),
        ConfigEntity::Category(c) => serde_yml::to_string(c),
        ConfigEntity::SearchFieldConfig(sfc) => serde_yml::to_string(sfc),
        ConfigEntity::Variable { key, value } => serde_yml::to_string(&VarYaml {
            key: key.clone(),
            value: value.clone(),
        }),
        ConfigEntity::Language(lang) => serde_yml::to_string(lang),
        ConfigEntity::GatherQuery(q) => {
            let export = GatherQueryExport {
                query_id: q.query_id.clone(),
                label: q.label.clone(),
                description: q.description.clone(),
                definition: serde_json::to_value(&q.definition).unwrap_or_default(),
                display: serde_json::to_value(&q.display).unwrap_or_default(),
                plugin: q.plugin.clone(),
            };
            serde_yml::to_string(&export)
        }
        ConfigEntity::UrlAlias(a) => serde_yml::to_string(a),
        ConfigEntity::Item(i) => serde_yml::to_string(i),
        // Roles need their permissions — callers must use serialize_role_entity.
        ConfigEntity::Role(role) => {
            warnings.push(format!(
                "serialize_entity called for role {} — use serialize_role_entity instead",
                role.id
            ));
            return None;
        }
        ConfigEntity::Stage(s) => serde_yml::to_string(s),
        ConfigEntity::Tile(t) => serde_yml::to_string(t),
        ConfigEntity::MenuLink(m) => serde_yml::to_string(m),
        // Tags need parent hierarchy — callers must use serialize_tag_entity.
        ConfigEntity::Tag(tag) => {
            warnings.push(format!(
                "serialize_entity called for tag {} — use serialize_tag_entity instead",
                tag.id
            ));
            return None;
        }
    };
    match result {
        Ok(yaml) => Some(yaml),
        Err(e) => {
            warnings.push(format!(
                "failed to serialize {} {id}: {e}",
                entity.entity_type()
            ));
            None
        }
    }
}

/// Serialize a role with its permissions to YAML.
///
/// The permissions are always written, including as an empty list: an exported
/// role is meant to be authoritative on re-import, and an omitted key means
/// "leave the grants alone" (see [`RoleExport`]).
fn serialize_role_entity(
    role: &Role,
    permissions: Vec<String>,
    warnings: &mut Vec<String>,
) -> Option<String> {
    let export = RoleExport {
        role: role.clone(),
        permissions: Some(permissions),
    };
    match serde_yml::to_string(&export) {
        Ok(yaml) => Some(yaml),
        Err(e) => {
            warnings.push(format!("failed to serialize role {}: {e}", role.id));
            None
        }
    }
}

/// Serialize a tag entity with parent hierarchy to YAML.
fn serialize_tag_entity(
    tag: &Tag,
    parent_ids: Vec<Uuid>,
    warnings: &mut Vec<String>,
) -> Option<String> {
    let export = TagExport {
        tag: tag.clone(),
        parents: parent_ids,
    };
    match serde_yml::to_string(&export) {
        Ok(yaml) => Some(yaml),
        Err(e) => {
            warnings.push(format!("failed to serialize tag {}: {e}", tag.id));
            None
        }
    }
}

/// Remove stale config files from a directory after export.
///
/// Only removes files that match the config filename pattern
/// (`{entity_type}.{id}.yml` or `.yaml`) and are NOT in the `keep` set.
/// Non-config files and freshly-written exports are preserved.
/// Since export always writes `.yml`, any `.yaml` variants are treated as stale.
///
/// Deletion failures are collected as warnings (not fatal) because
/// the export itself has already succeeded by the time this runs.
async fn clean_stale_yml_files(
    dir: &Path,
    keep: &HashSet<String>,
    warnings: &mut Vec<String>,
) -> Result<()> {
    let mut entries = tokio::fs::read_dir(dir)
        .await
        .with_context(|| format!("failed to read directory {}", dir.display()))?;

    while let Some(entry) = entries.next_entry().await? {
        let path = entry.path();
        if let Some(name) = path.file_name().and_then(|n| n.to_str())
            && parse_config_filename(name).is_some()
            && !keep.contains(name)
            && let Err(e) = tokio::fs::remove_file(&path).await
        {
            warnings.push(format!(
                "failed to remove stale file {}: {e}",
                path.display()
            ));
        }
    }
    Ok(())
}

/// Export all config entities to YAML files in the given directory.
///
/// All exported files use the `.yml` extension. When `clean` is true, removes
/// stale config files (both `.yml` and `.yaml`) from the directory *after*
/// writing new exports. Only files matching the config filename pattern that
/// were not written in this export are removed — this prevents data loss if
/// the export partially fails.
pub async fn export_config(
    storage: &dyn ConfigStorage,
    pool: &PgPool,
    dir: &Path,
    clean: bool,
) -> Result<ConfigOpResult> {
    info!(dir = %dir.display(), clean, "Starting config export");

    tokio::fs::create_dir_all(dir)
        .await
        .with_context(|| format!("failed to create directory {}", dir.display()))?;

    let mut result = ConfigOpResult::default();
    let mut written_files: HashSet<String> = HashSet::new();

    for &entity_type in ENTITY_TYPE_ORDER {
        let entities = storage
            .list(entity_type, None)
            .await
            .with_context(|| format!("failed to list {entity_type} entities"))?;

        let mut count = 0usize;

        for entity in entities {
            let id = entity.id();

            if let Err(e) = validate_entity_id_for_filename(&id) {
                result
                    .warnings
                    .push(format!("skipping {entity_type} with unsafe ID: {e}"));
                continue;
            }

            let filename = entity_filename(entity_type, &id);
            let path = dir.join(&filename);

            let yaml = match &entity {
                ConfigEntity::Tag(tag) => {
                    let parent_ids = match Tag::get_parents(pool, tag.id).await {
                        Ok(p) => p.into_iter().map(|t| t.id).collect(),
                        Err(e) => {
                            result
                                .warnings
                                .push(format!("failed to get parents for tag {id}: {e}"));
                            Vec::new()
                        }
                    };
                    match serialize_tag_entity(tag, parent_ids, &mut result.warnings) {
                        Some(yaml) => yaml,
                        None => continue,
                    }
                }
                ConfigEntity::Role(role) => {
                    let permissions = match Role::get_permissions(pool, role.id).await {
                        Ok(permissions) => permissions,
                        Err(e) => {
                            result
                                .warnings
                                .push(format!("failed to get permissions for role {id}: {e}"));
                            Vec::new()
                        }
                    };
                    match serialize_role_entity(role, permissions, &mut result.warnings) {
                        Some(yaml) => yaml,
                        None => continue,
                    }
                }
                other => match serialize_entity(other, &mut result.warnings) {
                    Some(yaml) => yaml,
                    None => continue,
                },
            };

            match tokio::fs::write(&path, &yaml).await {
                Ok(()) => {
                    count += 1;
                    written_files.insert(filename);
                }
                Err(e) => {
                    result
                        .warnings
                        .push(format!("failed to write {}: {e}", path.display()));
                }
            }
        }

        if count > 0 {
            debug!(entity_type, count, "Exported entity type");
            result.counts.insert(entity_type.to_string(), count);
        }
    }

    if clean && let Err(e) = clean_stale_yml_files(dir, &written_files, &mut result.warnings).await
    {
        result
            .warnings
            .push(format!("failed to clean stale files: {e}"));
    }

    if result.total() == 0 {
        info!("No config entities found in database");
    }

    info!(total = result.total(), "Config export complete");

    Ok(result)
}

/// Import config entities from YAML files in the given directory.
///
/// Validate first, then apply:
/// 1. **Parse pass**: every `.yml` file in the set is read, parsed and schema
///    checked against its config entity type.
/// 2. **Reference pass**: every reference an entity makes (a tag's category, a
///    search field config's bundle, a tag's parents) is resolved against the
///    import set and, failing that, the database. Reads only.
/// 3. **Save pass**: parsed entities are written to storage in dependency order.
///
/// If any file fails phase 1 or 2, this returns [`ConfigImportFailed`] naming
/// every failure and **nothing is written**. That makes a config set atomic with
/// respect to bad input: a single typo can no longer produce a partial import
/// that reports success. A failure in phase 3 is also reported as an error, but
/// earlier writes in that phase stand — see the module docs on transaction
/// safety — so the fix is to re-run once the named files are corrected.
///
/// When `dry_run` is true, phases 1 and 2 run and phase 3 is skipped, which
/// makes `--dry-run` a true preflight for the whole set.
///
/// Import is idempotent — `ConfigStorage::save()` performs upsert, so
/// re-running import on a partially-imported database converges correctly.
pub async fn import_config(
    storage: &dyn ConfigStorage,
    pool: &PgPool,
    dir: &Path,
    dry_run: bool,
) -> Result<ConfigOpResult> {
    info!(dir = %dir.display(), dry_run, "Starting config import");

    let mut result = ConfigOpResult::default();
    let mut failures: Vec<ConfigImportFailure> = Vec::new();

    // Phase 1: read, parse and schema check every file. No write happens until
    // the whole set has passed, so one bad file cannot leave a half-applied
    // config behind.
    let parsed = read_and_validate_files(dir, &mut result.warnings, &mut failures).await?;
    let parsed_total: usize = parsed.values().map(|v| v.len()).sum();
    debug!(
        files = parsed_total,
        failures = failures.len(),
        warnings = result.warnings.len(),
        "Parse pass complete"
    );

    // Phase 2: resolve every reference the set makes, before writing anything.
    validate_references(storage, pool, &parsed, &mut failures).await;

    if !failures.is_empty() {
        return Err(ConfigImportFailed {
            failures,
            imported: BTreeMap::new(),
        }
        .into());
    }

    if dry_run {
        for (entity_type, entities) in &parsed {
            if !entities.is_empty() {
                result.counts.insert(entity_type.clone(), entities.len());
            }
        }
        info!(total = result.total(), "Config import dry run complete");
        return Ok(result);
    }

    // Phase 3: save in dependency order. Everything here has already been
    // validated, so a failure means the storage layer rejected a valid entity.
    let mut tag_parents: Vec<(String, Uuid, Vec<Uuid>)> = Vec::new();
    let mut role_grants: Vec<(String, Uuid, Vec<String>)> = Vec::new();

    for &entity_type in ENTITY_TYPE_ORDER {
        let Some(entities) = parsed.get(entity_type) else {
            continue;
        };

        // `menu_link.parent_id` is a foreign key onto the same table, so a child
        // saved before its parent is rejected by the database. The group arrives
        // sorted by filename, which says nothing about tree order.
        let ordered: Vec<&ParsedEntity> = if entity_type == entity_types::MENU_LINK {
            order_menu_links_parents_first(entities)
        } else {
            entities.iter().collect()
        };

        let mut count = 0usize;

        for pe in ordered {
            if let Err(e) = storage.save(&pe.entity).await {
                failures.push(ConfigImportFailure {
                    filename: pe.filename.clone(),
                    error: format!("failed to save: {e:#}"),
                });
                continue;
            }
            count += 1;

            if let Some(permissions) = pe.role_permissions.as_ref() {
                match pe.entity.id().parse::<Uuid>() {
                    Ok(role_id) => {
                        role_grants.push((pe.filename.clone(), role_id, permissions.clone()));
                    }
                    Err(e) => {
                        failures.push(ConfigImportFailure {
                            filename: pe.filename.clone(),
                            error: format!("role ID is not a valid UUID: {e}"),
                        });
                    }
                }
            }

            if !pe.tag_parents.is_empty() {
                match pe.entity.id().parse::<Uuid>() {
                    Ok(tag_id) => {
                        tag_parents.push((pe.filename.clone(), tag_id, pe.tag_parents.clone()));
                    }
                    Err(e) => {
                        failures.push(ConfigImportFailure {
                            filename: pe.filename.clone(),
                            error: format!("tag ID is not a valid UUID: {e}"),
                        });
                    }
                }
            }
        }

        if count > 0 {
            debug!(entity_type, count, "Imported entity type");
            result.counts.insert(entity_type.to_string(), count);
        }
    }

    // Apply role permissions. Replace semantics, so the file is authoritative:
    // a permission it does not name is revoked. A file that omits the key entirely
    // is not in this list at all and leaves the role's grants untouched.
    for (filename, role_id, permissions) in &role_grants {
        if let Err(e) = Role::set_permissions(pool, *role_id, permissions).await {
            failures.push(ConfigImportFailure {
                filename: filename.clone(),
                error: format!("failed to set permissions for role {role_id}: {e:#}"),
            });
        }
    }

    // Restore tag hierarchy. Parents were resolved in phase 2, so anything that
    // fails here is a storage failure rather than a bad reference.
    for (filename, tag_id, parent_ids) in &tag_parents {
        if let Err(e) = Tag::set_parents(pool, *tag_id, parent_ids).await {
            failures.push(ConfigImportFailure {
                filename: filename.clone(),
                error: format!("failed to set parents for tag {tag_id}: {e:#}"),
            });
        }
    }

    if !failures.is_empty() {
        return Err(ConfigImportFailed {
            failures,
            imported: result.counts,
        }
        .into());
    }

    info!(total = result.total(), "Config import complete");

    Ok(result)
}

/// Resolve every reference the import set makes, against the set and then the
/// database. Reads only — this runs before the first write so that an
/// unresolvable reference aborts the whole import instead of silently skipping
/// one entity.
async fn validate_references(
    storage: &dyn ConfigStorage,
    pool: &PgPool,
    parsed: &BTreeMap<String, Vec<ParsedEntity>>,
    failures: &mut Vec<ConfigImportFailure>,
) {
    let known_categories: HashSet<String> = parsed
        .get(entity_types::CATEGORY)
        .map(|cats| {
            cats.iter()
                .filter_map(|pe| pe.entity.as_category().map(|c| c.id.clone()))
                .collect()
        })
        .unwrap_or_default();
    let known_item_types: HashSet<String> = parsed
        .get(entity_types::ITEM_TYPE)
        .map(|its| {
            its.iter()
                .filter_map(|pe| pe.entity.as_item_type().map(|it| it.type_name.clone()))
                .collect()
        })
        .unwrap_or_default();
    let known_tags: HashSet<Uuid> = parsed
        .get(entity_types::TAG)
        .map(|tags| {
            tags.iter()
                .filter_map(|pe| pe.entity.as_tag().map(|t| t.id))
                .collect()
        })
        .unwrap_or_default();

    if let Some(tags) = parsed.get(entity_types::TAG) {
        for pe in tags {
            let Some(tag) = pe.entity.as_tag() else {
                continue;
            };

            if !known_categories.contains(&tag.category_id) {
                match storage.load(entity_types::CATEGORY, &tag.category_id).await {
                    Ok(Some(_)) => {}
                    Ok(None) => failures.push(ConfigImportFailure {
                        filename: pe.filename.clone(),
                        error: format!(
                            "category '{}' not found in import set or database",
                            tag.category_id
                        ),
                    }),
                    Err(e) => failures.push(ConfigImportFailure {
                        filename: pe.filename.clone(),
                        error: format!("failed to verify category '{}': {e:#}", tag.category_id),
                    }),
                }
            }

            for parent_id in &pe.tag_parents {
                if *parent_id == tag.id {
                    failures.push(ConfigImportFailure {
                        filename: pe.filename.clone(),
                        error: "tag references itself as parent".to_string(),
                    });
                    continue;
                }
                if known_tags.contains(parent_id) {
                    continue;
                }
                match storage
                    .load(entity_types::TAG, &parent_id.to_string())
                    .await
                {
                    Ok(Some(_)) => {}
                    Ok(None) => failures.push(ConfigImportFailure {
                        filename: pe.filename.clone(),
                        error: format!(
                            "parent tag {parent_id} not found in import set or database"
                        ),
                    }),
                    Err(e) => failures.push(ConfigImportFailure {
                        filename: pe.filename.clone(),
                        error: format!("failed to verify parent tag {parent_id}: {e:#}"),
                    }),
                }
            }
        }
    }

    if let Some(configs) = parsed.get(entity_types::SEARCH_FIELD_CONFIG) {
        for pe in configs {
            let Some(sfc) = pe.entity.as_search_field_config() else {
                continue;
            };
            if known_item_types.contains(&sfc.bundle) {
                continue;
            }
            match storage.load(entity_types::ITEM_TYPE, &sfc.bundle).await {
                Ok(Some(_)) => {}
                Ok(None) => failures.push(ConfigImportFailure {
                    filename: pe.filename.clone(),
                    error: format!(
                        "bundle '{}' not found in import set or database",
                        sfc.bundle
                    ),
                }),
                Err(e) => failures.push(ConfigImportFailure {
                    filename: pe.filename.clone(),
                    error: format!("failed to verify bundle '{}': {e:#}", sfc.bundle),
                }),
            }
        }
    }

    validate_menu_link_parents(storage, parsed, failures).await;
    validate_role_permissions(pool, parsed, failures).await;
}

/// Reject a role file that names a permission the kernel cannot account for.
///
/// A permission is a bare string in `role_permissions`, so a typo is not a
/// constraint violation: it is a grant that silently never matches anything the
/// code checks. Since `config import` is now how a role gets its permissions, that
/// typo has to be caught here or not at all.
///
/// Valid means one of two things:
///
/// - a permission the kernel defines ([`crate::models::role::KERNEL_PERMISSIONS`]);
/// - a permission some role in this database already holds. A plugin declares its
///   permissions through `tap_perm`, which the kernel does not yet dispatch, so a
///   plugin's permissions are in no list the kernel can consult. Accepting what is
///   already granted is what lets an export of such a site re-import, and it is
///   also why the seeded `authenticated user` role, which holds `view own
///   profile`, does not trip this.
///
/// The message says which of the two likely causes it is, because "unknown
/// permission" on its own does not tell an operator whether to fix a typo or
/// enable a plugin.
async fn validate_role_permissions(
    pool: &PgPool,
    parsed: &BTreeMap<String, Vec<ParsedEntity>>,
    failures: &mut Vec<ConfigImportFailure>,
) {
    let Some(roles) = parsed.get(entity_types::ROLE) else {
        return;
    };
    if roles.iter().all(|pe| pe.role_permissions.is_none()) {
        return;
    }

    let mut known: HashSet<String> = crate::models::role::KERNEL_PERMISSIONS
        .iter()
        .map(|p| (*p).to_string())
        .collect();
    match Role::all_granted_permissions(pool).await {
        Ok(granted) => known.extend(granted),
        Err(e) => {
            // Without the granted set, a plugin's permission would be rejected as
            // unknown. Failing the import is the safe answer: silently narrowing
            // what counts as valid would revoke grants the file meant to keep.
            failures.push(ConfigImportFailure {
                filename: "(role permissions)".to_string(),
                error: format!("failed to read the existing permission grants: {e:#}"),
            });
            return;
        }
    }

    for pe in roles {
        let Some(permissions) = pe.role_permissions.as_ref() else {
            continue;
        };
        for permission in permissions {
            if permission.trim().is_empty() {
                failures.push(ConfigImportFailure {
                    filename: pe.filename.clone(),
                    error: "permission name must not be empty".to_string(),
                });
                continue;
            }
            if known.contains(permission) {
                continue;
            }
            failures.push(ConfigImportFailure {
                filename: pe.filename.clone(),
                error: format!(
                    "unknown permission '{permission}': it is not one the kernel defines and no \
                     role in this database holds it. The likely cause is a plugin that declares \
                     it not being enabled yet, since the kernel cannot enumerate a plugin's \
                     permissions; otherwise it is a typo."
                ),
            });
        }
    }
}

/// A menu link's `parent_id` for a link in this import set.
fn menu_link_of(pe: &ParsedEntity) -> Option<&MenuLink> {
    match &pe.entity {
        ConfigEntity::MenuLink(link) => Some(link),
        _ => None,
    }
}

/// Resolve every menu link's declared parent, and reject a parent chain that
/// loops.
///
/// Without this, a missing parent surfaces as a foreign-key rejection partway
/// through the save pass, reported against whichever file happened to be saved
/// first, and a cycle has no natural error at all: the foreign key permits one,
/// so the tree would import and then be unrenderable.
///
/// The walk stops when the chain leaves the import set. A link whose parent is
/// already a row is rooted in the database's own tree, which every path that
/// writes it keeps acyclic, so following it further would not tell us anything
/// this pass could act on.
async fn validate_menu_link_parents(
    storage: &dyn ConfigStorage,
    parsed: &BTreeMap<String, Vec<ParsedEntity>>,
    failures: &mut Vec<ConfigImportFailure>,
) {
    let Some(links) = parsed.get(entity_types::MENU_LINK) else {
        return;
    };

    let by_id: HashMap<Uuid, &MenuLink> = links
        .iter()
        .filter_map(menu_link_of)
        .map(|link| (link.id, link))
        .collect();

    for pe in links {
        let Some(link) = menu_link_of(pe) else {
            continue;
        };
        let Some(parent_id) = link.parent_id else {
            continue;
        };

        if parent_id == link.id {
            failures.push(ConfigImportFailure {
                filename: pe.filename.clone(),
                error: "menu link references itself as parent".to_string(),
            });
            continue;
        }

        if !by_id.contains_key(&parent_id) {
            match storage
                .load(entity_types::MENU_LINK, &parent_id.to_string())
                .await
            {
                Ok(Some(_)) => {}
                Ok(None) => failures.push(ConfigImportFailure {
                    filename: pe.filename.clone(),
                    error: format!(
                        "parent menu link {parent_id} not found in import set or database"
                    ),
                }),
                Err(e) => failures.push(ConfigImportFailure {
                    filename: pe.filename.clone(),
                    error: format!("failed to verify parent menu link {parent_id}: {e:#}"),
                }),
            }
            continue;
        }

        // Walk up within the set. Revisiting an id means the chain loops.
        let mut seen: HashSet<Uuid> = HashSet::from([link.id]);
        let mut cursor = parent_id;
        loop {
            if !seen.insert(cursor) {
                failures.push(ConfigImportFailure {
                    filename: pe.filename.clone(),
                    error: format!(
                        "menu link {} is its own ancestor: the parent chain forms a cycle",
                        link.id
                    ),
                });
                break;
            }
            match by_id.get(&cursor).and_then(|parent| parent.parent_id) {
                Some(next) => cursor = next,
                None => break,
            }
        }
    }
}

/// Order a menu-link import group so that a link's parent is always saved first.
///
/// A link whose parent is not in the set is ready immediately: validation has
/// already established that such a parent is an existing row.
///
/// Cycles are rejected during validation, so this always makes progress. It
/// still emits whatever is left if it ever does not: an ordering helper that can
/// spin is worse than one that hands the database an order it rejects with a
/// message.
fn order_menu_links_parents_first(entities: &[ParsedEntity]) -> Vec<&ParsedEntity> {
    let in_set: HashSet<Uuid> = entities
        .iter()
        .filter_map(menu_link_of)
        .map(|link| link.id)
        .collect();

    let mut ordered: Vec<&ParsedEntity> = Vec::with_capacity(entities.len());
    let mut saved: HashSet<Uuid> = HashSet::new();
    let mut pending: Vec<&ParsedEntity> = entities.iter().collect();

    while !pending.is_empty() {
        let mut progressed = false;
        let mut still_pending: Vec<&ParsedEntity> = Vec::with_capacity(pending.len());

        for pe in pending {
            let ready = match menu_link_of(pe) {
                Some(link) => match link.parent_id {
                    Some(parent) => !in_set.contains(&parent) || saved.contains(&parent),
                    None => true,
                },
                // Not a menu link: nothing to order it against.
                None => true,
            };

            if ready {
                if let Some(link) = menu_link_of(pe) {
                    saved.insert(link.id);
                }
                ordered.push(pe);
                progressed = true;
            } else {
                still_pending.push(pe);
            }
        }

        if !progressed {
            ordered.extend(still_pending);
            break;
        }
        pending = still_pending;
    }

    ordered
}

/// A parsed entity with metadata from its source file.
struct ParsedEntity {
    filename: String,
    entity: ConfigEntity,
    tag_parents: Vec<Uuid>,
    /// A role's declared permissions. `None` when the file omits the key, which
    /// means "leave this role's grants alone" rather than "revoke them all".
    role_permissions: Option<Vec<String>>,
}

/// Read all `.yml` files from a directory and validate/parse them.
///
/// Returns entities grouped by type, sorted by filename within each group
/// for deterministic ordering.
///
/// The two output channels are deliberately different in kind:
///
/// - `failures` collects anything that means a recognized config file cannot be
///   applied — an unreadable file, invalid YAML, or content that does not match
///   its entity type's schema. These abort the import in [`import_config`].
/// - `warnings` collects advisory observations that do not stop the import: a
///   file the config set does not claim (unrecognized prefix, symlink,
///   oversized), a filename whose ID disagrees with its content, or a duplicate
///   entity ID.
async fn read_and_validate_files(
    dir: &Path,
    warnings: &mut Vec<String>,
    failures: &mut Vec<ConfigImportFailure>,
) -> Result<BTreeMap<String, Vec<ParsedEntity>>> {
    let mut grouped: BTreeMap<String, Vec<ParsedEntity>> = BTreeMap::new();

    let mut entries = tokio::fs::read_dir(dir)
        .await
        .with_context(|| format!("failed to read directory {}", dir.display()))?;

    while let Some(entry) = entries.next_entry().await? {
        let path = entry.path();

        let Some(os_name) = path.file_name() else {
            continue;
        };
        let filename = match os_name.to_str() {
            Some(n) if !n.starts_with('.') && (n.ends_with(".yml") || n.ends_with(".yaml")) => {
                n.to_string()
            }
            Some(_) => continue, // non-matching filename, silently skip
            None => {
                warnings.push(format!(
                    "skipping file with non-UTF-8 name: {}",
                    path.display()
                ));
                continue;
            }
        };

        // Skip symlinks to prevent reading files outside the config directory
        let metadata = match tokio::fs::symlink_metadata(&path).await {
            Ok(m) => m,
            Err(e) => {
                warnings.push(format!("failed to read metadata for {filename}: {e}"));
                continue;
            }
        };
        if metadata.file_type().is_symlink() {
            warnings.push(format!("skipping symlink: {filename}"));
            continue;
        }

        // Reject excessively large files to prevent unbounded memory allocation
        if metadata.len() > MAX_CONFIG_FILE_SIZE {
            warnings.push(format!(
                "skipping {filename}: file size {} bytes exceeds limit of {} bytes",
                metadata.len(),
                MAX_CONFIG_FILE_SIZE
            ));
            continue;
        }

        let Some((entity_type, filename_id)) = parse_config_filename(&filename) else {
            warnings.push(format!("skipping unrecognized file: {filename}"));
            continue;
        };
        let entity_type = entity_type.to_string();
        let filename_id = filename_id.to_string();

        let content = match tokio::fs::read_to_string(&path).await {
            Ok(c) => c,
            Err(e) => {
                failures.push(ConfigImportFailure {
                    filename,
                    error: format!("failed to read: {e}"),
                });
                continue;
            }
        };

        let parsed = match deserialize_entity(&entity_type, &content) {
            Ok(result) => result,
            Err(e) => {
                failures.push(ConfigImportFailure {
                    filename,
                    error: format!("{e:#}"),
                });
                continue;
            }
        };

        // Validate filename-content ID consistency
        let content_id = parsed.entity.id();
        if content_id != filename_id {
            warnings.push(format!(
                "{filename}: filename ID '{filename_id}' does not match content ID '{content_id}'"
            ));
        }

        grouped.entry(entity_type).or_default().push(ParsedEntity {
            filename,
            entity: parsed.entity,
            tag_parents: parsed.tag_parents,
            role_permissions: parsed.role_permissions,
        });
    }

    // Sort each group by filename for deterministic ordering, then deduplicate
    for (entity_type, entities) in grouped.iter_mut() {
        entities.sort_by(|a, b| a.filename.cmp(&b.filename));

        // Detect duplicate entities (same content ID within a type group)
        let all = std::mem::take(entities);
        let mut seen_ids: HashSet<String> = HashSet::new();
        for pe in all {
            let id = pe.entity.id();
            if seen_ids.insert(id.clone()) {
                entities.push(pe);
            } else {
                warnings.push(format!(
                    "{}: duplicate {} entity with ID '{id}' (skipped)",
                    pe.filename, entity_type
                ));
            }
        }
    }

    Ok(grouped)
}

/// Deserialize YAML content into a ConfigEntity based on entity type.
///
/// Returns the parsed entity and any tag parent UUIDs (empty for non-tag types).
/// One config file's contents: the entity, plus what its file carries that the
/// entity struct does not.
///
/// A tuple was fine while the only such thing was a tag's parents. A role's
/// permissions are a second, so it is a struct with a name on each field.
#[derive(Debug)]
struct ParsedFile {
    entity: ConfigEntity,
    /// A tag's `parents`.
    tag_parents: Vec<Uuid>,
    /// A role's `permissions`; `None` when the file omits the key.
    role_permissions: Option<Vec<String>>,
}

impl ParsedFile {
    /// An entity whose file carries nothing beyond the entity itself.
    fn plain(entity: ConfigEntity) -> Self {
        Self {
            entity,
            tag_parents: Vec::new(),
            role_permissions: None,
        }
    }
}

fn deserialize_entity(entity_type: &str, content: &str) -> Result<ParsedFile> {
    match entity_type {
        entity_types::VARIABLE => {
            let var: VarYaml = serde_yml::from_str(content).context("invalid variable YAML")?;
            if var.key.is_empty() {
                anyhow::bail!("variable key must not be empty");
            }
            Ok(ParsedFile::plain(ConfigEntity::Variable {
                key: var.key,
                value: var.value,
            }))
        }
        entity_types::ITEM_TYPE => {
            let item_type: ItemType =
                serde_yml::from_str(content).context("invalid item_type YAML")?;
            Ok(ParsedFile::plain(ConfigEntity::ItemType(item_type)))
        }
        entity_types::CATEGORY => {
            let category: Category =
                serde_yml::from_str(content).context("invalid category YAML")?;
            Ok(ParsedFile::plain(ConfigEntity::Category(category)))
        }
        entity_types::TAG => {
            let export: TagExport = serde_yml::from_str(content).context("invalid tag YAML")?;
            Ok(ParsedFile {
                entity: ConfigEntity::Tag(export.tag),
                tag_parents: export.parents,
                role_permissions: None,
            })
        }
        entity_types::SEARCH_FIELD_CONFIG => {
            let sfc: SearchFieldConfig =
                serde_yml::from_str(content).context("invalid search_field_config YAML")?;
            Ok(ParsedFile::plain(ConfigEntity::SearchFieldConfig(sfc)))
        }
        entity_types::LANGUAGE => {
            let lang: Language = serde_yml::from_str(content).context("invalid language YAML")?;
            Ok(ParsedFile::plain(ConfigEntity::Language(lang)))
        }
        entity_types::GATHER_QUERY => {
            let export: GatherQueryExport =
                serde_yml::from_str(content).context("invalid gather_query YAML")?;
            if export.query_id.is_empty() {
                anyhow::bail!("gather_query query_id must not be empty");
            }
            let definition = serde_json::from_value(export.definition)
                .context("invalid gather_query definition")?;
            let display =
                serde_json::from_value(export.display).context("invalid gather_query display")?;
            let query = GatherQuery {
                query_id: export.query_id,
                label: export.label,
                description: export.description,
                definition,
                display,
                plugin: export.plugin,
                created: 0,
                changed: 0,
            };
            for route in &query.display.routes {
                if route.path.is_empty() || !route.path.starts_with('/') {
                    anyhow::bail!(
                        "gather query '{}' has invalid route path '{}': must start with '/'",
                        query.query_id,
                        route.path,
                    );
                }
            }
            Ok(ParsedFile::plain(ConfigEntity::GatherQuery(Box::new(
                query,
            ))))
        }
        entity_types::URL_ALIAS => {
            let alias: UrlAlias = serde_yml::from_str(content).context("invalid url_alias YAML")?;
            Ok(ParsedFile::plain(ConfigEntity::UrlAlias(alias)))
        }
        entity_types::ITEM => {
            let item: super::ConfigItem =
                serde_yml::from_str(content).context("invalid item YAML")?;
            Ok(ParsedFile::plain(ConfigEntity::Item(item)))
        }
        entity_types::ROLE => {
            let export: RoleExport = serde_yml::from_str(content).context("invalid role YAML")?;
            Ok(ParsedFile {
                entity: ConfigEntity::Role(export.role),
                tag_parents: Vec::new(),
                role_permissions: export.permissions,
            })
        }
        entity_types::STAGE => {
            let stage: Stage = serde_yml::from_str(content).context("invalid stage YAML")?;
            Ok(ParsedFile::plain(ConfigEntity::Stage(stage)))
        }
        entity_types::TILE => {
            let tile: Tile = serde_yml::from_str(content).context("invalid tile YAML")?;
            Ok(ParsedFile::plain(ConfigEntity::Tile(tile)))
        }
        entity_types::MENU_LINK => {
            let link: MenuLink = serde_yml::from_str(content).context("invalid menu_link YAML")?;
            Ok(ParsedFile::plain(ConfigEntity::MenuLink(link)))
        }
        _ => anyhow::bail!("unknown entity type: {entity_type}"),
    }
}

#[cfg(test)]
// Tests are allowed to use unwrap/expect freely.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    use std::ops::Deref;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEST_DIR_COUNTER: AtomicU64 = AtomicU64::new(0);

    /// RAII guard for test directories. Automatically removes the directory
    /// on drop, guaranteeing cleanup even if the test panics.
    struct TestDir(std::path::PathBuf);

    impl TestDir {
        fn new(name: &str) -> Self {
            let n = TEST_DIR_COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir()
                .join(format!("trovato_test_{name}_{n}_{}", std::process::id()));
            // Remove leftovers from a previous run, if any
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Deref for TestDir {
        type Target = std::path::Path;
        fn deref(&self) -> &std::path::Path {
            &self.0
        }
    }

    impl AsRef<std::path::Path> for TestDir {
        fn as_ref(&self) -> &std::path::Path {
            &self.0
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    // ── Filename parsing ───────────────────────────────────────────

    #[test]
    fn parse_config_filename_item_type() {
        let result = parse_config_filename("item_type.blog.yml");
        assert_eq!(result, Some(("item_type", "blog")));
    }

    #[test]
    fn parse_config_filename_tag_uuid() {
        let result = parse_config_filename("tag.019483a7-b1c2-7def-8012-abcdef123456.yml");
        assert_eq!(
            result,
            Some(("tag", "019483a7-b1c2-7def-8012-abcdef123456"))
        );
    }

    #[test]
    fn parse_config_filename_variable() {
        let result = parse_config_filename("variable.site_name.yml");
        assert_eq!(result, Some(("variable", "site_name")));
    }

    #[test]
    fn parse_config_filename_search_field_config() {
        let result =
            parse_config_filename("search_field_config.019483a7-b1c2-7def-8012-abcdef789012.yml");
        assert_eq!(
            result,
            Some((
                "search_field_config",
                "019483a7-b1c2-7def-8012-abcdef789012"
            ))
        );
    }

    #[test]
    fn parse_config_filename_bad_no_dot() {
        assert_eq!(parse_config_filename("bad-filename.yml"), None);
    }

    #[test]
    fn parse_config_filename_unknown_type() {
        assert_eq!(parse_config_filename("unknown_type.foo.yml"), None);
    }

    #[test]
    fn parse_config_filename_yaml_extension() {
        assert_eq!(
            parse_config_filename("item_type.blog.yaml"),
            Some(("item_type", "blog"))
        );
    }

    #[test]
    fn parse_config_filename_no_extension() {
        assert_eq!(parse_config_filename("item_type.blog.json"), None);
    }

    #[test]
    fn parse_config_filename_empty_id() {
        assert_eq!(parse_config_filename("item_type..yml"), None);
    }

    // ── Filename generation ────────────────────────────────────────

    #[test]
    fn entity_filename_generation() {
        assert_eq!(entity_filename("item_type", "blog"), "item_type.blog.yml");
        assert_eq!(
            entity_filename("variable", "site_name"),
            "variable.site_name.yml"
        );
        assert_eq!(
            entity_filename("tag", "019483a7-b1c2-7def-8012-abcdef123456"),
            "tag.019483a7-b1c2-7def-8012-abcdef123456.yml"
        );
    }

    // ── Entity ID validation ──────────────────────────────────────

    #[test]
    fn validate_entity_id_rejects_path_separators() {
        assert!(validate_entity_id_for_filename("../../etc/passwd").is_err());
        assert!(validate_entity_id_for_filename("foo/bar").is_err());
        assert!(validate_entity_id_for_filename("foo\\bar").is_err());
        assert!(validate_entity_id_for_filename("foo\0bar").is_err());
        assert!(validate_entity_id_for_filename("a..b").is_err());
        assert!(validate_entity_id_for_filename("").is_err());
    }

    #[test]
    fn validate_entity_id_rejects_windows_invalid_chars() {
        assert!(validate_entity_id_for_filename("foo:bar").is_err());
        assert!(validate_entity_id_for_filename("foo*bar").is_err());
        assert!(validate_entity_id_for_filename("foo?bar").is_err());
        assert!(validate_entity_id_for_filename("foo\"bar").is_err());
        assert!(validate_entity_id_for_filename("foo<bar").is_err());
        assert!(validate_entity_id_for_filename("foo>bar").is_err());
        assert!(validate_entity_id_for_filename("foo|bar").is_err());
    }

    #[test]
    fn validate_entity_id_rejects_leading_trailing_dots() {
        assert!(validate_entity_id_for_filename(".hidden").is_err());
        assert!(validate_entity_id_for_filename("trailing.").is_err());
        assert!(validate_entity_id_for_filename(".").is_err());
    }

    #[test]
    fn validate_entity_id_rejects_leading_trailing_whitespace() {
        assert!(validate_entity_id_for_filename(" leading").is_err());
        assert!(validate_entity_id_for_filename("trailing ").is_err());
        assert!(validate_entity_id_for_filename("\ttab").is_err());
        assert!(validate_entity_id_for_filename(" ").is_err());
    }

    #[test]
    fn validate_entity_id_accepts_safe_ids() {
        assert!(validate_entity_id_for_filename("blog").is_ok());
        assert!(validate_entity_id_for_filename("site_name").is_ok());
        assert!(validate_entity_id_for_filename("smtp.host").is_ok()); // dots OK, just not ".."
        assert!(validate_entity_id_for_filename("019483a7-b1c2-7def-8012-abcdef123456").is_ok());
    }

    // ── YAML round-trip tests ──────────────────────────────────────

    #[test]
    fn item_type_yaml_round_trip() {
        let item_type = ItemType {
            type_name: "blog".to_string(),
            label: "Blog Post".to_string(),
            description: Some("A blog article".to_string()),
            has_title: true,
            title_label: Some("Title".to_string()),
            plugin: "trovato_blog".to_string(),
            settings: serde_json::json!({"fields": []}),
        };

        let yaml = serde_yml::to_string(&item_type).unwrap();
        // ItemType uses #[serde(rename = "type")] on type_name
        assert!(
            yaml.contains("type: blog"),
            "Expected 'type: blog' in:\n{yaml}"
        );
        assert!(
            !yaml.contains("type_name"),
            "Should not contain type_name in:\n{yaml}"
        );

        let parsed: ItemType = serde_yml::from_str(&yaml).unwrap();
        assert_eq!(parsed.type_name, "blog");
        assert_eq!(parsed.label, "Blog Post");
        assert_eq!(parsed.description, Some("A blog article".to_string()));
        assert!(parsed.has_title);
    }

    #[test]
    fn category_yaml_round_trip() {
        let category = Category {
            id: "topics".to_string(),
            label: "Topics".to_string(),
            description: Some("Content topics".to_string()),
            hierarchy: 1,
            weight: 0,
        };

        let yaml = serde_yml::to_string(&category).unwrap();
        let parsed: Category = serde_yml::from_str(&yaml).unwrap();
        assert_eq!(parsed.id, "topics");
        assert_eq!(parsed.label, "Topics");
        assert_eq!(parsed.hierarchy, 1);
    }

    #[test]
    fn tag_export_yaml_round_trip_with_parents() {
        let tag = Tag {
            id: Uuid::parse_str("019483a7-b1c2-7def-8012-abcdef123456").unwrap(),
            category_id: "topics".to_string(),
            label: "Rust".to_string(),
            description: Some("Rust programming language".to_string()),
            slug: Some("rust".to_string()),
            weight: 0,
            created: 1708000000,
            changed: 1708000000,
        };
        let parent_id = Uuid::parse_str("019483a7-b1c2-7def-8012-aaa111111111").unwrap();

        let export = TagExport {
            tag: tag.clone(),
            parents: vec![parent_id],
        };

        let yaml = serde_yml::to_string(&export).unwrap();
        assert!(yaml.contains("parents:"), "Expected parents in:\n{yaml}");
        assert!(
            yaml.contains("aaa111111111"),
            "Expected parent UUID in:\n{yaml}"
        );

        let parsed: TagExport = serde_yml::from_str(&yaml).unwrap();
        assert_eq!(parsed.tag.id, tag.id);
        assert_eq!(parsed.tag.label, "Rust");
        assert_eq!(parsed.parents.len(), 1);
        assert_eq!(parsed.parents[0], parent_id);
    }

    #[test]
    fn tag_export_yaml_round_trip_no_parents() {
        let tag = Tag {
            id: Uuid::parse_str("019483a7-b1c2-7def-8012-abcdef123456").unwrap(),
            category_id: "topics".to_string(),
            label: "Rust".to_string(),
            description: None,
            slug: None,
            weight: 0,
            created: 1708000000,
            changed: 1708000000,
        };

        let export = TagExport {
            tag,
            parents: vec![],
        };

        let yaml = serde_yml::to_string(&export).unwrap();
        // parents should be omitted when empty (skip_serializing_if)
        assert!(
            !yaml.contains("parents"),
            "Empty parents should be omitted:\n{yaml}"
        );

        let parsed: TagExport = serde_yml::from_str(&yaml).unwrap();
        assert!(parsed.parents.is_empty());
    }

    #[test]
    fn search_field_config_yaml_round_trip() {
        let sfc = SearchFieldConfig {
            id: Uuid::parse_str("019483a7-b1c2-7def-8012-abcdef789012").unwrap(),
            bundle: "blog".to_string(),
            field_name: "body".to_string(),
            weight: "A".to_string(),
        };

        let yaml = serde_yml::to_string(&sfc).unwrap();
        let parsed: SearchFieldConfig = serde_yml::from_str(&yaml).unwrap();
        assert_eq!(parsed.id, sfc.id);
        assert_eq!(parsed.bundle, "blog");
        assert_eq!(parsed.field_name, "body");
        assert_eq!(parsed.weight, "A");
    }

    #[test]
    fn language_yaml_round_trip() {
        let lang = Language {
            id: "en".to_string(),
            label: "English".to_string(),
            weight: 0,
            is_default: true,
            direction: "ltr".to_string(),
        };

        let yaml = serde_yml::to_string(&lang).unwrap();
        assert!(yaml.contains("id: en"), "Expected 'id: en' in:\n{yaml}");

        let parsed: Language = serde_yml::from_str(&yaml).unwrap();
        assert_eq!(parsed.id, "en");
        assert_eq!(parsed.label, "English");
        assert!(parsed.is_default);
        assert_eq!(parsed.direction, "ltr");
    }

    #[test]
    fn variable_yaml_round_trip() {
        let var = VarYaml {
            key: "site_name".to_string(),
            value: serde_json::json!("My Site"),
        };

        let yaml = serde_yml::to_string(&var).unwrap();
        assert!(yaml.contains("key: site_name"), "Expected key in:\n{yaml}");

        let parsed: VarYaml = serde_yml::from_str(&yaml).unwrap();
        assert_eq!(parsed.key, "site_name");
        assert_eq!(parsed.value, serde_json::json!("My Site"));
    }

    // ── Ordering / constraint tests ────────────────────────────────

    #[test]
    fn import_order_matches_dependency_constraints() {
        let pos = |name: &str| ENTITY_TYPE_ORDER.iter().position(|&t| t == name).unwrap();

        // Languages must come before item_types (no FK, but logical ordering)
        assert!(
            pos(entity_types::LANGUAGE) < pos(entity_types::ITEM_TYPE),
            "languages must be imported before item_types"
        );

        // Categories must come before tags (FK constraint)
        assert!(
            pos(entity_types::CATEGORY) < pos(entity_types::TAG),
            "categories must be imported before tags"
        );

        // Item types must come before search field configs (bundle reference)
        assert!(
            pos(entity_types::ITEM_TYPE) < pos(entity_types::SEARCH_FIELD_CONFIG),
            "item_types must be imported before search_field_configs"
        );
    }

    #[test]
    fn entity_type_order_covers_all_known_types() {
        // Ensures ENTITY_TYPE_ORDER stays in sync with entity_types constants.
        let expected: HashSet<&str> = [
            entity_types::VARIABLE,
            entity_types::LANGUAGE,
            entity_types::ITEM_TYPE,
            entity_types::CATEGORY,
            entity_types::TAG,
            entity_types::SEARCH_FIELD_CONFIG,
            entity_types::GATHER_QUERY,
            entity_types::URL_ALIAS,
            entity_types::ITEM,
            entity_types::ROLE,
            entity_types::STAGE,
            entity_types::TILE,
            entity_types::MENU_LINK,
        ]
        .into_iter()
        .collect();

        let actual: HashSet<&str> = ENTITY_TYPE_ORDER.iter().copied().collect();

        assert_eq!(
            expected, actual,
            "ENTITY_TYPE_ORDER must contain exactly all entity_types constants"
        );
    }

    // ── Deserialization tests ──────────────────────────────────────

    #[test]
    fn deserialize_entity_variable() {
        let yaml = "key: site_name\nvalue: My Site\n";
        let parsed = deserialize_entity("variable", yaml).unwrap();
        let (entity, tag_parents) = (parsed.entity, parsed.tag_parents);
        assert_eq!(entity.entity_type(), "variable");
        assert_eq!(entity.id(), "site_name");
        assert!(tag_parents.is_empty());
    }

    #[test]
    fn deserialize_entity_item_type() {
        let yaml = r#"
type: blog
label: Blog Post
description: A blog article
has_title: true
title_label: Title
plugin: trovato_blog
settings: {}
"#;
        let parsed = deserialize_entity("item_type", yaml).unwrap();
        let (entity, tag_parents) = (parsed.entity, parsed.tag_parents);
        assert_eq!(entity.entity_type(), "item_type");
        assert_eq!(entity.id(), "blog");
        assert!(tag_parents.is_empty());
    }

    #[test]
    fn deserialize_entity_tag_with_parents() {
        let yaml = r#"
id: "019483a7-b1c2-7def-8012-abcdef123456"
category_id: topics
label: Rust
description: Rust programming language
weight: 0
created: 1708000000
changed: 1708000000
parents:
  - "019483a7-b1c2-7def-8012-aaa111111111"
"#;
        let parsed = deserialize_entity("tag", yaml).unwrap();
        let (entity, tag_parents) = (parsed.entity, parsed.tag_parents);
        assert_eq!(entity.entity_type(), "tag");

        assert_eq!(tag_parents.len(), 1);
        assert_eq!(
            tag_parents[0],
            Uuid::parse_str("019483a7-b1c2-7def-8012-aaa111111111").unwrap()
        );
    }

    #[test]
    fn deserialize_entity_language() {
        let yaml = "id: fr\nlabel: French\nweight: 1\nis_default: false\ndirection: ltr\n";
        let parsed = deserialize_entity("language", yaml).unwrap();
        let (entity, tag_parents) = (parsed.entity, parsed.tag_parents);
        assert_eq!(entity.entity_type(), "language");
        assert_eq!(entity.id(), "fr");
        assert!(tag_parents.is_empty());
    }

    #[test]
    fn deserialize_entity_rejects_empty_variable_key() {
        let yaml = "key: \"\"\nvalue: test\n";
        let result = deserialize_entity("variable", yaml);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("empty"),
            "expected 'empty' in error: {err_msg}"
        );
    }

    #[test]
    fn deserialize_entity_unknown_type() {
        let result = deserialize_entity("bogus", "key: val\n");
        assert!(result.is_err());
    }

    #[test]
    fn tutorial_conference_yaml_deserializes() {
        let yaml = include_str!("../../../../docs/tutorial/config/item_type.conference.yml");
        let parsed = deserialize_entity("item_type", yaml).unwrap();
        let (entity, tag_parents) = (parsed.entity, parsed.tag_parents);
        assert_eq!(entity.entity_type(), "item_type");
        assert_eq!(entity.id(), "conference");
        assert!(tag_parents.is_empty());

        let it = entity.as_item_type().expect("expected ItemType variant");
        assert_eq!(it.label, "Conference");
        assert!(it.has_title);
        assert_eq!(it.title_label.as_deref(), Some("Conference Name"));
        assert_eq!(it.plugin, "core");

        // Verify all 14 fields deserialize with correct types and required flags
        let fields: Vec<trovato_sdk::types::FieldDefinition> = it
            .settings
            .get("fields")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .expect("settings.fields should deserialize");

        assert_eq!(fields.len(), 14, "expected 14 fields, got {}", fields.len());

        // Required fields
        let start = fields
            .iter()
            .find(|f| f.field_name == "field_start_date")
            .unwrap();
        assert!(start.required, "field_start_date should be required");
        let end = fields
            .iter()
            .find(|f| f.field_name == "field_end_date")
            .unwrap();
        assert!(end.required, "field_end_date should be required");

        // Non-required fields should default to false
        let city = fields
            .iter()
            .find(|f| f.field_name == "field_city")
            .unwrap();
        assert!(!city.required, "field_city should not be required");

        // Verify field type variants
        assert!(matches!(
            start.field_type,
            trovato_sdk::types::FieldType::Date
        ));
        assert!(matches!(
            city.field_type,
            trovato_sdk::types::FieldType::Text { .. }
        ));
        let online = fields
            .iter()
            .find(|f| f.field_name == "field_online")
            .unwrap();
        assert!(matches!(
            online.field_type,
            trovato_sdk::types::FieldType::Boolean
        ));
        let desc = fields
            .iter()
            .find(|f| f.field_name == "field_description")
            .unwrap();
        assert!(matches!(
            desc.field_type,
            trovato_sdk::types::FieldType::Blocks
        ));
    }

    // ── Serialize helper tests ─────────────────────────────────────

    #[test]
    fn serialize_entity_item_type_records_warning_not_panic() {
        // serialize_entity should never panic; it records warnings.
        let entity = ConfigEntity::ItemType(ItemType {
            type_name: "blog".to_string(),
            label: "Blog".to_string(),
            description: None,
            has_title: true,
            title_label: None,
            plugin: "trovato_blog".to_string(),
            settings: serde_json::json!({}),
        });

        let mut warnings = Vec::new();
        let yaml = serialize_entity(&entity, &mut warnings);
        assert!(yaml.is_some());
        assert!(warnings.is_empty());
    }

    #[test]
    fn serialize_entity_tag_returns_warning_not_panic() {
        let tag = Tag {
            id: Uuid::parse_str("019483a7-b1c2-7def-8012-abcdef123456").unwrap(),
            category_id: "topics".to_string(),
            label: "Rust".to_string(),
            description: None,
            slug: None,
            weight: 0,
            created: 1708000000,
            changed: 1708000000,
        };
        let entity = ConfigEntity::Tag(tag);

        let mut warnings = Vec::new();
        let yaml = serialize_entity(&entity, &mut warnings);
        assert!(yaml.is_none());
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("serialize_tag_entity"));
    }

    #[test]
    fn serialize_tag_entity_with_parents() {
        let tag = Tag {
            id: Uuid::parse_str("019483a7-b1c2-7def-8012-abcdef123456").unwrap(),
            category_id: "topics".to_string(),
            label: "Rust".to_string(),
            description: None,
            slug: None,
            weight: 0,
            created: 1708000000,
            changed: 1708000000,
        };
        let parent_id = Uuid::parse_str("019483a7-b1c2-7def-8012-aaa111111111").unwrap();

        let mut warnings = Vec::new();
        let yaml = serialize_tag_entity(&tag, vec![parent_id], &mut warnings).unwrap();
        assert!(yaml.contains("parents:"));
        assert!(yaml.contains("aaa111111111"));
        assert!(warnings.is_empty());
    }

    // ── Filesystem round-trip tests ────────────────────────────────
    //
    // NOTE: Integration tests for the full export_config/import_config flow
    // require a database and are covered in the integration test suite.

    #[tokio::test]
    async fn filesystem_round_trip_parse_written_files() {
        let dir = TestDir::new("roundtrip");

        // Write config files
        let item_type_yaml = "type: blog\nlabel: Blog\ndescription: null\nhas_title: true\ntitle_label: null\nplugin: trovato_blog\nsettings: {}\n";
        let variable_yaml = "key: site_name\nvalue: My Site\n";
        let category_yaml =
            "id: topics\nlabel: Topics\ndescription: null\nhierarchy: 0\nweight: 0\n";
        let language_yaml = "id: en\nlabel: English\nweight: 0\nis_default: true\ndirection: ltr\n";

        tokio::fs::write(dir.join("item_type.blog.yml"), item_type_yaml)
            .await
            .unwrap();
        tokio::fs::write(dir.join("variable.site_name.yml"), variable_yaml)
            .await
            .unwrap();
        tokio::fs::write(dir.join("category.topics.yml"), category_yaml)
            .await
            .unwrap();
        tokio::fs::write(dir.join("language.en.yml"), language_yaml)
            .await
            .unwrap();
        // Non-yml file should be ignored
        tokio::fs::write(dir.join("README.md"), "ignore me")
            .await
            .unwrap();

        let mut warnings = Vec::new();
        let mut failures = Vec::new();
        let parsed = read_and_validate_files(&dir, &mut warnings, &mut failures)
            .await
            .unwrap();

        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
        assert_eq!(parsed.get("item_type").unwrap().len(), 1);
        assert_eq!(parsed.get("variable").unwrap().len(), 1);
        assert_eq!(parsed.get("category").unwrap().len(), 1);
        assert_eq!(parsed.get("language").unwrap().len(), 1);

        let it = &parsed["item_type"][0];
        assert_eq!(it.entity.id(), "blog");
        assert_eq!(it.filename, "item_type.blog.yml");
    }

    #[tokio::test]
    async fn filesystem_skips_dotfiles() {
        let dir = TestDir::new("dotfiles");

        // Dotfile matching config pattern should be silently skipped (no warning)
        let yaml = "key: site_name\nvalue: My Site\n";
        tokio::fs::write(dir.join(".variable.site_name.yml"), yaml)
            .await
            .unwrap();
        // Normal file should be parsed
        tokio::fs::write(dir.join("variable.site_name.yml"), yaml)
            .await
            .unwrap();

        let mut warnings = Vec::new();
        let mut failures = Vec::new();
        let parsed = read_and_validate_files(&dir, &mut warnings, &mut failures)
            .await
            .unwrap();

        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
        assert_eq!(parsed.get("variable").unwrap().len(), 1);
    }

    #[tokio::test]
    async fn filesystem_accepts_yaml_extension() {
        let dir = TestDir::new("yaml_ext");

        let yaml = "key: site_name\nvalue: My Site\n";
        tokio::fs::write(dir.join("variable.site_name.yaml"), yaml)
            .await
            .unwrap();

        let mut warnings = Vec::new();
        let mut failures = Vec::new();
        let parsed = read_and_validate_files(&dir, &mut warnings, &mut failures)
            .await
            .unwrap();

        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
        assert_eq!(parsed.get("variable").unwrap().len(), 1);
    }

    #[tokio::test]
    async fn filesystem_warns_on_id_mismatch() {
        let dir = TestDir::new("mismatch");

        // Filename says "blog" but content says "page"
        let yaml = "type: page\nlabel: Page\ndescription: null\nhas_title: true\ntitle_label: null\nplugin: core\nsettings: {}\n";
        tokio::fs::write(dir.join("item_type.blog.yml"), yaml)
            .await
            .unwrap();

        let mut warnings = Vec::new();
        let mut failures = Vec::new();
        let parsed = read_and_validate_files(&dir, &mut warnings, &mut failures)
            .await
            .unwrap();

        // Entity should still be parsed (non-fatal)
        assert_eq!(parsed.get("item_type").unwrap().len(), 1);
        // But a warning should be emitted about the mismatch
        assert_eq!(warnings.len(), 1);
        assert!(
            warnings[0].contains("does not match"),
            "expected mismatch warning, got: {}",
            warnings[0]
        );
    }

    /// Malformed YAML is a failure, not a warning. A warning is something an
    /// operator can miss; this is a file that will not be applied.
    #[tokio::test]
    async fn filesystem_fails_on_bad_yaml() {
        let dir = TestDir::new("badyaml");

        tokio::fs::write(dir.join("variable.broken.yml"), "not: [valid: yaml: {}")
            .await
            .unwrap();

        let mut warnings = Vec::new();
        let mut failures = Vec::new();
        let parsed = read_and_validate_files(&dir, &mut warnings, &mut failures)
            .await
            .unwrap();

        assert!(!parsed.contains_key("variable") || parsed["variable"].is_empty());
        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
        assert_eq!(failures.len(), 1, "expected one failure: {failures:?}");
        assert_eq!(failures[0].filename, "variable.broken.yml");
    }

    /// A file that is valid YAML but does not match its entity type's schema is
    /// the same class of failure as malformed YAML: it cannot be applied. This is
    /// what every stage, role, tile and menu link in the tutorial config set hit.
    #[tokio::test]
    async fn filesystem_fails_on_schema_mismatch() {
        let dir = TestDir::new("schema_mismatch");

        // Valid YAML, but Stage requires `id` and `machine_name`.
        tokio::fs::write(
            dir.join("stage.incoming.yml"),
            "label: Incoming\nvisibility: internal\nis_default: false\nweight: 0\n",
        )
        .await
        .unwrap();

        let mut warnings = Vec::new();
        let mut failures = Vec::new();
        let parsed = read_and_validate_files(&dir, &mut warnings, &mut failures)
            .await
            .unwrap();

        assert!(!parsed.contains_key("stage") || parsed["stage"].is_empty());
        assert_eq!(failures.len(), 1, "expected one failure: {failures:?}");
        assert_eq!(failures[0].filename, "stage.incoming.yml");
        assert!(
            failures[0].error.contains("missing field"),
            "failure should name the missing field, got: {}",
            failures[0].error
        );
    }

    #[tokio::test]
    async fn clean_stale_removes_only_stale_config_files() {
        let dir = TestDir::new("clean_stale");

        // Stale config file (not in keep set)
        tokio::fs::write(dir.join("item_type.old.yml"), "stale")
            .await
            .unwrap();
        // Freshly written config file (in keep set)
        tokio::fs::write(dir.join("variable.site_name.yml"), "fresh")
            .await
            .unwrap();
        // Non-config file (should not be touched)
        tokio::fs::write(dir.join("README.md"), "keep me")
            .await
            .unwrap();
        // Non-config yml file (unrecognized prefix, should not be touched)
        tokio::fs::write(dir.join("notes.yml"), "keep me too")
            .await
            .unwrap();

        let keep: HashSet<String> = ["variable.site_name.yml".to_string()].into_iter().collect();
        let mut warnings = Vec::new();
        clean_stale_yml_files(&dir, &keep, &mut warnings)
            .await
            .unwrap();

        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
        assert!(
            !dir.join("item_type.old.yml").exists(),
            "stale config file should be removed"
        );
        assert!(
            dir.join("variable.site_name.yml").exists(),
            "fresh config file should be kept"
        );
        assert!(
            dir.join("README.md").exists(),
            "non-yml file should be kept"
        );
        assert!(
            dir.join("notes.yml").exists(),
            "non-config yml should be kept"
        );
    }

    #[tokio::test]
    async fn filesystem_deduplicates_entities_by_content_id() {
        let dir = TestDir::new("dedup");

        // Two files with different names but same content ID
        let yaml = "type: blog\nlabel: Blog\ndescription: null\nhas_title: true\ntitle_label: null\nplugin: trovato_blog\nsettings: {}\n";
        tokio::fs::write(dir.join("item_type.blog.yml"), yaml)
            .await
            .unwrap();
        // Filename says "other" but content ID is "blog" — same entity
        tokio::fs::write(dir.join("item_type.other.yml"), yaml)
            .await
            .unwrap();

        let mut warnings = Vec::new();
        let mut failures = Vec::new();
        let parsed = read_and_validate_files(&dir, &mut warnings, &mut failures)
            .await
            .unwrap();

        // Only one entity should survive deduplication
        assert_eq!(parsed.get("item_type").unwrap().len(), 1);
        // Should have mismatch warning for "other" file + duplicate warning
        let has_dup_warning = warnings.iter().any(|w| w.contains("duplicate"));
        assert!(
            has_dup_warning,
            "expected duplicate warning, got: {warnings:?}"
        );
    }

    #[tokio::test]
    async fn filesystem_results_are_sorted_by_filename() {
        let dir = TestDir::new("sorted");

        // Create files that would be unsorted by filesystem enumeration
        tokio::fs::write(
            dir.join("variable.zzz_last.yml"),
            "key: zzz_last\nvalue: z\n",
        )
        .await
        .unwrap();
        tokio::fs::write(
            dir.join("variable.aaa_first.yml"),
            "key: aaa_first\nvalue: a\n",
        )
        .await
        .unwrap();

        let mut warnings = Vec::new();
        let mut failures = Vec::new();
        let parsed = read_and_validate_files(&dir, &mut warnings, &mut failures)
            .await
            .unwrap();

        let vars = parsed.get("variable").unwrap();
        assert_eq!(vars.len(), 2);
        assert_eq!(vars[0].filename, "variable.aaa_first.yml");
        assert_eq!(vars[1].filename, "variable.zzz_last.yml");
    }
}
