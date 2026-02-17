//! YAML-based config export/import.
//!
//! Exports all config entities to individual YAML files (one per entity)
//! and re-imports them. File naming: `{entity_type}.{id}.yml`.
//!
//! Import is idempotent: `ConfigStorage::save()` performs upsert, so
//! re-running import on a partially-imported database converges to the
//! correct state.
//!
//! # Transaction Safety
//!
//! Import saves entities individually, not in a single database transaction,
//! because [`ConfigStorage`] is a trait that may wrap different backends.
//! If the process is interrupted mid-import, simply re-run the import to
//! converge to the correct state.

use std::collections::{BTreeMap, HashSet};
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use tracing::{debug, info};
use uuid::Uuid;

use super::{ConfigEntity, ConfigStorage, SearchFieldConfig, entity_types};
use crate::models::{Category, ItemType, Language, Tag};

/// Entity type ordering used for both validation and dependency-ordered import.
///
/// A single source of truth: earlier entries are imported first.
/// Categories before tags (FK), item_types before search_field_configs (bundle ref).
const ENTITY_TYPE_ORDER: &[&str] = &[
    entity_types::VARIABLE,
    entity_types::LANGUAGE,
    entity_types::ITEM_TYPE,
    entity_types::CATEGORY,
    entity_types::TAG,
    entity_types::SEARCH_FIELD_CONFIG,
];

/// Maximum config file size (10 MB). Files exceeding this are skipped during import
/// to prevent unbounded memory allocation from malicious or accidental large files.
const MAX_CONFIG_FILE_SIZE: u64 = 10 * 1024 * 1024;

/// Characters that are invalid in filenames on Windows/NTFS.
/// Rejected by [`validate_entity_id_for_filename`] for cross-platform portability.
const WINDOWS_INVALID_CHARS: &[char] = &[':', '*', '?', '"', '<', '>', '|'];

/// Tag with hierarchy parents for export/import.
#[derive(Serialize, Deserialize)]
struct TagExport {
    #[serde(flatten)]
    tag: Tag,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    parents: Vec<Uuid>,
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
/// Performs a two-phase approach:
/// 1. **Validation pass**: reads and parses all YAML files, checking for errors.
/// 2. **Save pass**: writes parsed entities to storage in dependency order.
///
/// When `dry_run` is true, only the validation pass runs (no database writes).
///
/// Import is idempotent — `ConfigStorage::save()` performs upsert, so
/// re-running import on a partially-imported database converges correctly.
///
/// # Transaction Safety
///
/// Entities are saved individually, not in a single database transaction,
/// because [`ConfigStorage`] is a trait that may wrap different backends.
/// If the process is interrupted mid-import, re-run the import to converge.
pub async fn import_config(
    storage: &dyn ConfigStorage,
    pool: &PgPool,
    dir: &Path,
    dry_run: bool,
) -> Result<ConfigOpResult> {
    info!(dir = %dir.display(), dry_run, "Starting config import");

    let mut result = ConfigOpResult::default();

    // Phase 1: Read and validate all files
    let parsed = read_and_validate_files(dir, &mut result.warnings).await?;
    let parsed_total: usize = parsed.values().map(|v| v.len()).sum();
    debug!(
        files = parsed_total,
        warnings = result.warnings.len(),
        "Validation complete"
    );

    if dry_run {
        // Count what would be imported
        for (entity_type, entities) in &parsed {
            if !entities.is_empty() {
                result.counts.insert(entity_type.clone(), entities.len());
            }
        }

        // Validate tag hierarchy references within the import set
        if let Some(tag_entities) = parsed.get(entity_types::TAG) {
            let all_tag_ids: HashSet<Uuid> = tag_entities
                .iter()
                .filter_map(|pe| pe.entity.as_tag().map(|t| t.id))
                .collect();
            for pe in tag_entities {
                let tag_id = pe.entity.as_tag().map(|t| t.id);
                for parent_id in &pe.tag_parents {
                    if tag_id == Some(*parent_id) {
                        result
                            .warnings
                            .push(format!("{}: tag references itself as parent", pe.filename));
                    } else if !all_tag_ids.contains(parent_id) {
                        result.warnings.push(format!(
                            "{}: parent tag {} not found in import set (may exist in database)",
                            pe.filename, parent_id
                        ));
                    }
                }
            }
        }

        return Ok(result);
    }

    // Phase 2: Save in dependency order
    // Build reference sets from parsed entities for pre-save validation
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

    // Track successfully saved tag IDs (only tags that actually saved)
    let mut saved_tag_ids: HashSet<Uuid> = HashSet::new();
    // Track tags that need hierarchy restoration
    let mut tag_parents: Vec<(Uuid, Vec<Uuid>)> = Vec::new();

    for &entity_type in ENTITY_TYPE_ORDER {
        let entities = match parsed.get(entity_type) {
            Some(e) => e,
            None => continue,
        };

        let mut count = 0usize;

        for pe in entities {
            // Pre-validate foreign key references
            if entity_type == entity_types::TAG
                && let Some(tag) = pe.entity.as_tag()
                && !known_categories.contains(&tag.category_id)
            {
                // Check database as fallback
                match storage.load(entity_types::CATEGORY, &tag.category_id).await {
                    Ok(Some(_)) => {} // exists in DB
                    Ok(None) => {
                        result.warnings.push(format!(
                            "{}: category '{}' not found in import set or database (skipping tag)",
                            pe.filename, tag.category_id
                        ));
                        continue;
                    }
                    Err(e) => {
                        result.warnings.push(format!(
                            "{}: failed to verify category '{}': {e}",
                            pe.filename, tag.category_id
                        ));
                        continue;
                    }
                }
            }
            if entity_type == entity_types::SEARCH_FIELD_CONFIG
                && let Some(sfc) = pe.entity.as_search_field_config()
                && !known_item_types.contains(&sfc.bundle)
            {
                match storage.load(entity_types::ITEM_TYPE, &sfc.bundle).await {
                    Ok(Some(_)) => {} // exists in DB
                    Ok(None) => {
                        result.warnings.push(format!(
                            "{}: bundle '{}' not found in import set or database (skipping search_field_config)",
                            pe.filename, sfc.bundle
                        ));
                        continue;
                    }
                    Err(e) => {
                        result.warnings.push(format!(
                            "{}: failed to verify bundle '{}': {e}",
                            pe.filename, sfc.bundle
                        ));
                        continue;
                    }
                }
            }

            if let Err(e) = storage.save(&pe.entity).await {
                result
                    .warnings
                    .push(format!("failed to save {}: {e}", pe.filename));
                continue;
            }
            count += 1;

            // Track successfully saved tags
            if entity_type == entity_types::TAG
                && let Ok(tag_id) = pe.entity.id().parse::<Uuid>()
            {
                saved_tag_ids.insert(tag_id);
            }

            // Collect tag parents for hierarchy restoration
            if !pe.tag_parents.is_empty() {
                match pe.entity.id().parse::<Uuid>() {
                    Ok(tag_id) => {
                        tag_parents.push((tag_id, pe.tag_parents.clone()));
                    }
                    Err(e) => {
                        result
                            .warnings
                            .push(format!("{}: tag ID is not a valid UUID: {e}", pe.filename));
                    }
                }
            }
        }

        if count > 0 {
            debug!(entity_type, count, "Imported entity type");
            result.counts.insert(entity_type.to_string(), count);
        }
    }

    // Restore tag hierarchy with parent validation
    for (tag_id, parent_ids) in &tag_parents {
        // Reject self-referencing parents
        let parent_ids: Vec<Uuid> = parent_ids
            .iter()
            .filter(|pid| {
                if *pid == tag_id {
                    result
                        .warnings
                        .push(format!("tag {tag_id}: ignoring self-referencing parent"));
                    false
                } else {
                    true
                }
            })
            .copied()
            .collect();

        if parent_ids.is_empty() {
            continue;
        }

        // Check parents not in the saved set against the database
        let not_in_import: Vec<&Uuid> = parent_ids
            .iter()
            .filter(|pid| !saved_tag_ids.contains(pid))
            .collect();

        let mut truly_missing: HashSet<Uuid> = HashSet::new();
        for pid in not_in_import {
            match storage.load(entity_types::TAG, &pid.to_string()).await {
                Ok(Some(_)) => {} // exists in database, fine
                Ok(None) => {
                    truly_missing.insert(*pid);
                }
                Err(e) => {
                    result
                        .warnings
                        .push(format!("tag {tag_id}: failed to verify parent {pid}: {e}"));
                }
            }
        }

        if !truly_missing.is_empty() {
            let missing_strs: Vec<String> = truly_missing.iter().map(|u| u.to_string()).collect();
            result.warnings.push(format!(
                "tag {tag_id}: parent(s) not found in import set or database: {}",
                missing_strs.join(", ")
            ));
        }

        // Filter out missing parents to avoid creating orphaned hierarchy rows
        let valid_parents: Vec<Uuid> = parent_ids
            .iter()
            .filter(|pid| !truly_missing.contains(pid))
            .copied()
            .collect();

        if !valid_parents.is_empty()
            && let Err(e) = Tag::set_parents(pool, *tag_id, &valid_parents).await
        {
            result
                .warnings
                .push(format!("failed to set parents for tag {tag_id}: {e}"));
        }
    }

    info!(total = result.total(), "Config import complete");

    Ok(result)
}

/// A parsed entity with metadata from its source file.
struct ParsedEntity {
    filename: String,
    entity: ConfigEntity,
    tag_parents: Vec<Uuid>,
}

/// Read all `.yml` files from a directory and validate/parse them.
///
/// Returns entities grouped by type, sorted by filename within each group
/// for deterministic ordering. Duplicate entities (same type and content ID)
/// are detected and skipped with a warning. Parse errors and filename-content
/// ID mismatches are also recorded as warnings (not fatal).
async fn read_and_validate_files(
    dir: &Path,
    warnings: &mut Vec<String>,
) -> Result<BTreeMap<String, Vec<ParsedEntity>>> {
    let mut grouped: BTreeMap<String, Vec<ParsedEntity>> = BTreeMap::new();

    let mut entries = tokio::fs::read_dir(dir)
        .await
        .with_context(|| format!("failed to read directory {}", dir.display()))?;

    while let Some(entry) = entries.next_entry().await? {
        let path = entry.path();

        let os_name = match path.file_name() {
            Some(n) => n,
            None => continue,
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

        let (entity_type, filename_id) = match parse_config_filename(&filename) {
            Some(parsed) => parsed,
            None => {
                warnings.push(format!("skipping unrecognized file: {filename}"));
                continue;
            }
        };
        let entity_type = entity_type.to_string();
        let filename_id = filename_id.to_string();

        let content = match tokio::fs::read_to_string(&path).await {
            Ok(c) => c,
            Err(e) => {
                warnings.push(format!("failed to read {}: {e}", path.display()));
                continue;
            }
        };

        let (entity, tag_parents) = match deserialize_entity(&entity_type, &content) {
            Ok(result) => result,
            Err(e) => {
                warnings.push(format!("failed to parse {filename}: {e}"));
                continue;
            }
        };

        // Validate filename-content ID consistency
        let content_id = entity.id();
        if content_id != filename_id {
            warnings.push(format!(
                "{filename}: filename ID '{filename_id}' does not match content ID '{content_id}'"
            ));
        }

        grouped.entry(entity_type).or_default().push(ParsedEntity {
            filename,
            entity,
            tag_parents,
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
fn deserialize_entity(entity_type: &str, content: &str) -> Result<(ConfigEntity, Vec<Uuid>)> {
    match entity_type {
        entity_types::VARIABLE => {
            let var: VarYaml = serde_yml::from_str(content).context("invalid variable YAML")?;
            if var.key.is_empty() {
                anyhow::bail!("variable key must not be empty");
            }
            Ok((
                ConfigEntity::Variable {
                    key: var.key,
                    value: var.value,
                },
                Vec::new(),
            ))
        }
        entity_types::ITEM_TYPE => {
            let item_type: ItemType =
                serde_yml::from_str(content).context("invalid item_type YAML")?;
            Ok((ConfigEntity::ItemType(item_type), Vec::new()))
        }
        entity_types::CATEGORY => {
            let category: Category =
                serde_yml::from_str(content).context("invalid category YAML")?;
            Ok((ConfigEntity::Category(category), Vec::new()))
        }
        entity_types::TAG => {
            let export: TagExport = serde_yml::from_str(content).context("invalid tag YAML")?;
            Ok((ConfigEntity::Tag(export.tag), export.parents))
        }
        entity_types::SEARCH_FIELD_CONFIG => {
            let sfc: SearchFieldConfig =
                serde_yml::from_str(content).context("invalid search_field_config YAML")?;
            Ok((ConfigEntity::SearchFieldConfig(sfc), Vec::new()))
        }
        entity_types::LANGUAGE => {
            let lang: Language = serde_yml::from_str(content).context("invalid language YAML")?;
            Ok((ConfigEntity::Language(lang), Vec::new()))
        }
        _ => anyhow::bail!("unknown entity type: {entity_type}"),
    }
}

#[cfg(test)]
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
            plugin: "blog".to_string(),
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
        let (entity, tag_parents) = deserialize_entity("variable", yaml).unwrap();
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
plugin: blog
settings: {}
"#;
        let (entity, tag_parents) = deserialize_entity("item_type", yaml).unwrap();
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
        let (entity, tag_parents) = deserialize_entity("tag", yaml).unwrap();
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
        let (entity, tag_parents) = deserialize_entity("language", yaml).unwrap();
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
            plugin: "blog".to_string(),
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
        let item_type_yaml = "type: blog\nlabel: Blog\ndescription: null\nhas_title: true\ntitle_label: null\nplugin: blog\nsettings: {}\n";
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
        let parsed = read_and_validate_files(&dir, &mut warnings).await.unwrap();

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
        let parsed = read_and_validate_files(&dir, &mut warnings).await.unwrap();

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
        let parsed = read_and_validate_files(&dir, &mut warnings).await.unwrap();

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
        let parsed = read_and_validate_files(&dir, &mut warnings).await.unwrap();

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

    #[tokio::test]
    async fn filesystem_warns_on_bad_yaml() {
        let dir = TestDir::new("badyaml");

        tokio::fs::write(dir.join("variable.broken.yml"), "not: [valid: yaml: {}")
            .await
            .unwrap();

        let mut warnings = Vec::new();
        let parsed = read_and_validate_files(&dir, &mut warnings).await.unwrap();

        assert!(parsed.get("variable").is_none() || parsed["variable"].is_empty());
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("failed to parse"));
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
        let yaml = "type: blog\nlabel: Blog\ndescription: null\nhas_title: true\ntitle_label: null\nplugin: blog\nsettings: {}\n";
        tokio::fs::write(dir.join("item_type.blog.yml"), yaml)
            .await
            .unwrap();
        // Filename says "other" but content ID is "blog" — same entity
        tokio::fs::write(dir.join("item_type.other.yml"), yaml)
            .await
            .unwrap();

        let mut warnings = Vec::new();
        let parsed = read_and_validate_files(&dir, &mut warnings).await.unwrap();

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
        let parsed = read_and_validate_files(&dir, &mut warnings).await.unwrap();

        let vars = parsed.get("variable").unwrap();
        assert_eq!(vars.len(), 2);
        assert_eq!(vars[0].filename, "variable.aaa_first.yml");
        assert_eq!(vars[1].filename, "variable.zzz_last.yml");
    }
}
