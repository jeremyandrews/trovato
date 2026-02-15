# Story 21.2: Config Revision Schema

Status: done

<!-- Note: Validation is optional. Run validate-create-story for quality check before dev-story. -->

## Story

As an **architect**,
I want config revision tables in the schema,
So that adding config staging later is schema-ready.

## Acceptance Criteria

1. **Migration creates `config_revision` table:**
   ```sql
   CREATE TABLE config_revision (
       id UUID PRIMARY KEY,
       entity_type VARCHAR(64) NOT NULL,
       entity_id VARCHAR(255) NOT NULL,  -- String to match ConfigEntity.id() return type
       data JSONB NOT NULL,
       created BIGINT NOT NULL,
       author_id UUID REFERENCES users(id)
   );
   CREATE INDEX idx_config_revision_entity ON config_revision(entity_type, entity_id);
   CREATE INDEX idx_config_revision_created ON config_revision(created DESC);
   ```

2. **Migration creates `config_stage_association` table:**
   ```sql
   CREATE TABLE config_stage_association (
       stage_id VARCHAR(64) NOT NULL,
       entity_type VARCHAR(64) NOT NULL,
       entity_id VARCHAR(255) NOT NULL,
       target_revision_id UUID NOT NULL REFERENCES config_revision(id),
       PRIMARY KEY (stage_id, entity_type, entity_id)
   );
   ```

3. **`stage_deletion` table already supports config entities** - verify `entity_type` column exists or add if missing

4. **No v1.0 code writes to these tables** - they're scaffolding for post-MVP

5. **Schema documented** with comments explaining future purpose

## Tasks / Subtasks

- [x] Task 1: Create migration file for config_revision table (AC: #1)
  - [x] Create `crates/kernel/migrations/20260216000001_create_config_revision.sql`
  - [x] Define table with id, entity_type, entity_id, data, created, author_id
  - [x] Add indexes for entity lookup and time-ordered queries
  - [x] Add SQL comments documenting future purpose

- [x] Task 2: Create migration file for config_stage_association table (AC: #2)
  - [x] Create `crates/kernel/migrations/20260216000002_create_config_stage_association.sql`
  - [x] Define composite primary key (stage_id, entity_type, entity_id)
  - [x] Add FK to config_revision(id)
  - [x] Note: No FK to stage table (stage table doesn't exist yet, item.stage_id is just VARCHAR)

- [x] Task 3: Verify or create stage_deletion table (AC: #3)
  - [x] Check if stage_deletion table exists in migrations
  - [x] If missing, create migration with entity_type support
  - [x] Ensure it can track deleted config entities per stage

- [x] Task 4: Run migrations and verify (AC: #4)
  - [x] Run `sqlx migrate run` or `cargo sqlx migrate run`
  - [x] Verify tables created with correct schema
  - [x] Verify no data is written (tables should be empty)

- [x] Task 5: Add tests (AC: #4, #5)
  - [x] Integration test verifying tables exist
  - [x] Test that ConfigStorage does NOT write to these tables in v1.0

## Dev Notes

### Purpose

This story creates **scaffolding only** - no runtime code changes. The tables exist so that:
1. Post-MVP `StageAwareConfigStorage` can write config revisions
2. The schema doesn't require migration when staging is enabled
3. Config entities (ItemType, Category, Tag, Variable, SearchFieldConfig) can be versioned

### Relationship to Story 21.1

Story 21.1 created `ConfigStorage` trait and `DirectConfigStorage`. Post-MVP:
- `StageAwareConfigStorage` will wrap `DirectConfigStorage`
- When saving a config entity in a stage, it writes to `config_revision`
- `config_stage_association` links stage → entity → revision
- On publish, the revision becomes the live version

### Entity ID Format

The `entity_id` column is `VARCHAR(255)` (not UUID) because `ConfigEntity.id()` returns `String`:
- ItemType: `"page"` (type_name)
- Category: `"tags"` (string id)
- Tag: `"550e8400-..."` (UUID as string)
- Variable: `"site_name"` (key)
- SearchFieldConfig: `"550e8400-..."` (UUID as string)

### Stage Table Note

The `item` table has `stage_id VARCHAR(64)` but there's no `stage` table yet. The `config_stage_association` table also uses `stage_id VARCHAR(64)` without FK. When stage management UI is added (Story 21.3+), a proper `stage` table will be created and FKs added.

### Migration Naming Convention

Existing migrations follow pattern: `YYYYMMDD00000N_description.sql`
- Latest: `20260215000001_create_site_config.sql`
- This story: `20260216000001_create_config_revision.sql`, `20260216000002_...`

### Testing Strategy

Since tables are empty scaffolding:
1. **Schema test**: Verify tables exist with correct columns
2. **Negative test**: Verify `DirectConfigStorage` operations don't touch these tables
3. **No CRUD tests** - tables aren't used in v1.0

### Project Structure Notes

- Migrations: `crates/kernel/migrations/`
- SQLx manages migrations automatically on startup via `db::run_migrations()`

### References

- [Source: _bmad-output/planning-artifacts/epics.md#Story 21.2]
- [Source: crates/kernel/migrations/20260212000005_create_items.sql] - stage_id pattern
- [Source: crates/kernel/src/config_storage/mod.rs] - ConfigEntity.id() return type
- [Source: Story 21.1] - ConfigStorage trait foundation

## Dev Agent Record

### Agent Model Used

### Debug Log References

### Completion Notes List

### File List

**New Files:**
- `crates/kernel/migrations/20260216000001_create_config_revision.sql` - config_revision table for versioned config snapshots
- `crates/kernel/migrations/20260216000002_create_config_stage_association.sql` - junction table linking stages to config revisions
- `crates/kernel/migrations/20260216000003_create_stage_deletion.sql` - tracks deletions within stages before publishing

**Modified Files:**
- `crates/kernel/tests/config_storage_test.rs` - added 2 tests: `config_revision_schema_tables_exist`, `config_storage_does_not_write_to_revision_tables`

## Change Log

- 2026-02-14: Story created via create-story workflow
- 2026-02-14: Implementation complete - all 3 migrations and 2 tests pass
