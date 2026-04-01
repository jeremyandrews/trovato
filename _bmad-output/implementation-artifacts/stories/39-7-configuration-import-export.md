# Story 39.7: Configuration Import/Export

Status: done

## Story

As a **site administrator**,
I want to export all site configuration to YAML files and re-import it on another instance,
so that I can version-control configuration, replicate environments, and recover from configuration drift.

## Acceptance Criteria

1. 13 entity types exportable: variable, language, role, item_type, category, tag, search_field_config, gather_query, stage, url_alias, item, tile, menu_link
2. Export writes one YAML file per entity: `{entity_type}.{id}.yml`
3. Import reads YAML files in dependency order (categories before tags, item_types before search_field_configs)
4. Import uses upsert semantics via `ConfigStorage::save()` -- re-running import converges to correct state
5. Dry-run mode validates files without persisting changes
6. Entity IDs validated for filename safety: no `..`, path separators, or Windows-invalid characters
7. Config file size limit (10MB) prevents resource exhaustion from malicious imports
8. Tag exports include hierarchy parent UUIDs for tree reconstruction on import
9. Gather query exports use opaque JSON for definition/display fields to avoid YAML tag round-trip issues

## Tasks / Subtasks

- [x] Define `ENTITY_TYPE_ORDER` constant as single source of truth for dependency ordering (AC: #3)
- [x] Implement YAML export: iterate all entities from `ConfigStorage`, serialize to individual YAML files (AC: #1, #2)
- [x] Implement YAML import: read files from directory, parse entity type and ID from filename, load in dependency order (AC: #3)
- [x] Implement upsert semantics: `ConfigStorage::save()` creates or updates (AC: #4)
- [x] Implement dry-run mode that validates and reports without persisting (AC: #5)
- [x] Implement `validate_entity_id_for_filename()` rejecting traversal attacks and invalid characters (AC: #6)
- [x] Implement config file size limit check (10MB max) during import (AC: #7)
- [x] Define `TagExport` struct with flattened Tag + parent UUID list for hierarchy preservation (AC: #8)
- [x] Define `GatherQueryExport` struct using `serde_json::Value` for definition/display to avoid YAML tag issues (AC: #9)
- [x] Add unit tests for YAML round-trip, validation, dependency ordering, and edge cases (AC: #1-#9)

## Dev Notes

### Architecture

The YAML config system provides a complete configuration-as-code workflow. Each config entity is exported as an individual YAML file named `{entity_type}.{entity_id}.yml`, making it Git-friendly (one file per entity = clean diffs).

Import ordering is critical for referential integrity. `ENTITY_TYPE_ORDER` defines a single array used for both validation and import sequencing: variables and languages first (no dependencies), then roles, item_types, categories, tags (FK to category), search_field_configs (reference item_type bundles), gather_queries, stages, url_aliases, items, tiles, and menu_links last.

The import is intentionally non-transactional: `ConfigStorage` is a trait that may wrap different backends, so wrapping everything in a single DB transaction is not possible. Instead, idempotent upsert semantics allow safe re-execution -- if import is interrupted, just run it again to converge.

Gather queries use `serde_json::Value` for their definition/display fields rather than typed structs because complex serde enum types (e.g., `ContextualValue::UrlArg`) serialize as YAML tags (`!url_arg`) that don't round-trip through standard YAML parsers.

### Testing

- Unit tests in `crates/kernel/src/config_storage/yaml.rs` (34 tests) -- round-trip serialization, filename validation, dependency ordering, size limits, dry-run, edge cases
- Integration tests exercise full export/import cycles against real database

### References

- `crates/kernel/src/config_storage/yaml.rs` (1,674 lines) -- YAML export/import, validation, entity type ordering
- `crates/kernel/src/config_storage/direct.rs` (1,173 lines) -- DirectConfigStorage providing upsert semantics
- `crates/kernel/src/config_storage/mod.rs` -- ConfigStorage trait, ConfigEntity enum
