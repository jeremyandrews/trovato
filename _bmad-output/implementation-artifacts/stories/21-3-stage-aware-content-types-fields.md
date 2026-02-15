# Story 21.3: Stage-Aware Content Types & Fields

Status: done

## Story

As a **site administrator**,
I want to add/modify content types and fields in a stage,
So that I can test schema changes before going live.

## Acceptance Criteria

1. **Given** I create a content type in a stage
   **When** I view that stage
   **Then** the type exists and I can create items of that type
   **And** the type does not exist in Live

2. **Given** I add a field to an existing content type in a stage
   **When** I view items in that stage
   **Then** the field appears on edit forms
   **And** Live items don't have the field

3. **Given** I publish a stage with content type changes
   **When** publishing completes
   **Then** the content type/field exists in Live
   **And** existing items get default values for new fields

4. Stage preview shows content forms with staged field configuration

## Tasks / Subtasks

- [ ] Task 1: Create StageAwareConfigStorage decorator
  - [ ] Create `crates/kernel/src/config_storage/stage_aware.rs`
  - [ ] Implement ConfigStorage trait with stage context
  - [ ] Load: check stage first, fall back to live
  - [ ] Save: write to config_revision and config_stage_association
  - [ ] Delete: record in stage_deletion table
  - [ ] List: merge stage and live results

- [ ] Task 2: Add stage context to config operations
  - [ ] Add `with_stage()` method to get stage-scoped storage
  - [ ] Wire into AppState for session-based stage access

- [ ] Task 3: Wire item_type through stage-aware path
  - [ ] Update ContentTypeRegistry to use stage-aware config
  - [ ] Handle type creation/modification in stage

- [ ] Task 4: Add publish phase for config_types
  - [ ] Implement phase 1 callback in stage publish
  - [ ] Copy config revisions to live tables on publish

- [ ] Task 5: Add integration tests
  - [ ] Test creating content type in stage
  - [ ] Test type not visible in live until publish
  - [ ] Test publish makes type live

## Dev Notes

### StageAwareConfigStorage Design

```rust
pub struct StageAwareConfigStorage {
    direct: DirectConfigStorage,
    stage_id: String,
    pool: PgPool,
}

impl ConfigStorage for StageAwareConfigStorage {
    async fn load(&self, entity_type: &str, id: &str) -> Result<Option<ConfigEntity>> {
        // 1. Check if entity is deleted in this stage
        if self.is_deleted(entity_type, id).await? {
            return Ok(None);
        }

        // 2. Check for staged revision
        if let Some(revision) = self.get_staged_revision(entity_type, id).await? {
            return Ok(Some(deserialize_revision(revision)));
        }

        // 3. Fall back to live
        self.direct.load(entity_type, id).await
    }
}
```

### References

- Story 21.1: ConfigStorage trait (foundation)
- Story 21.2: config_revision, config_stage_association tables

## Dev Agent Record

### File List

### Change Log

- 2026-02-14: Story created
