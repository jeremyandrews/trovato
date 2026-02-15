# Story 21.7: Atomic Publish Ordering Framework

Status: done

## Story

As an **architect**,
I want a publish ordering framework,
So that config and content publish in the correct dependency order.

## Acceptance Criteria

1. **Publish order is defined and enforced:**
   ```
   Phase 1: Content types, fields (nothing depends on these)
   Phase 2: Categories (may be referenced by content)
   Phase 3: Content items (depend on types and categories existing)
   Phase 4: Menus, aliases (reference content)
   ```

2. **All phases run in single Postgres transaction**

3. **If any phase fails, entire transaction rolls back**

4. **v1.0: Only Phase 3 (items) is active - other phases are no-op hooks**
   Post-MVP: Wire up other phases as config staging is added

5. **Cache invalidation follows same ordering (invalidate after all writes)**

6. **Publish function accepts phase callbacks:**
   ```rust
   pub struct PublishPhases {
       pub config_types: Box<dyn Fn(&mut Transaction) -> Result<()>>,
       pub categories: Box<dyn Fn(&mut Transaction) -> Result<()>>,
       pub items: Box<dyn Fn(&mut Transaction) -> Result<()>>,
       pub dependents: Box<dyn Fn(&mut Transaction) -> Result<()>>,
   }
   ```

## Tasks / Subtasks

- [x] Task 1: Create stage module with PublishPhases structure
  - [x] Create `crates/kernel/src/stage/mod.rs`
  - [x] Define `PublishPhase` enum for the 4 phases
  - [x] Define `PublishPhases` struct with phase callbacks
  - [x] Define `PublishResult` with success/failure info

- [x] Task 2: Implement phased publish function
  - [x] Create `publish_stage()` function
  - [x] Execute phases in order within single transaction
  - [x] Roll back on any phase failure
  - [x] Return detailed result with which phases succeeded

- [x] Task 3: Implement items phase (v1.0 active phase)
  - [x] Create `publish_items()` function
  - [x] Copy staged items to live (update stage_id)
  - [x] Track deletion records from stage_deletion table

- [x] Task 4: Add no-op hooks for other phases
  - [x] Create placeholder functions for config_types, categories, dependents
  - [x] Document these are post-MVP implementations

- [x] Task 5: Add cache invalidation
  - [x] Call `cache.invalidate_stage()` after successful publish
  - [x] Ensure invalidation happens AFTER transaction commits

- [x] Task 6: Integrate with AppState
  - [x] Add StageService to AppState
  - [x] Export stage module from lib.rs

- [x] Task 7: Add integration tests
  - [x] Test successful publish with items phase
  - [x] Test rollback on failure
  - [x] Test cache invalidation

## Dev Notes

### Purpose

This story creates the **publish ordering framework** for stages. In v1.0:
- Only items phase is active (moves staged items to live)
- Other phases are no-op placeholders
- The framework enables post-MVP config staging

### Current State

- Items have `stage_id` column (default 'live')
- Session tracks active stage
- Cache supports stage isolation via `stage_key()`
- Cache has `invalidate_stage()` for publish cleanup
- **No publish function exists yet**

### Phase Ordering Rationale

```
Phase 1: Config types/fields → nothing depends on these
Phase 2: Categories → content may reference them
Phase 3: Items → depend on types and categories
Phase 4: Menus/aliases → reference content items
```

This ordering prevents "item published before its type exists" errors.

### Implementation Approach

```rust
pub async fn publish_stage(
    pool: &PgPool,
    stage_id: &str,
    phases: PublishPhases,
) -> Result<PublishResult> {
    let mut tx = pool.begin().await?;

    // Phase 1: Config types (no-op in v1.0)
    (phases.config_types)(&mut tx)?;

    // Phase 2: Categories (no-op in v1.0)
    (phases.categories)(&mut tx)?;

    // Phase 3: Items (active in v1.0)
    (phases.items)(&mut tx)?;

    // Phase 4: Dependents (no-op in v1.0)
    (phases.dependents)(&mut tx)?;

    tx.commit().await?;
    Ok(PublishResult::success())
}
```

### References

- [Source: crates/kernel/src/cache/mod.rs:172-237] - invalidate_stage function
- [Source: crates/kernel/src/models/item.rs] - stage_id field
- [Source: crates/kernel/migrations/20260216000003_create_stage_deletion.sql] - deletion tracking

## Dev Agent Record

### Agent Model Used

### Debug Log References

### Completion Notes List

### File List

**New Files:**
- `crates/kernel/src/stage/mod.rs` - StageService with phased publish framework
- `crates/kernel/tests/stage_test.rs` - 7 integration tests for stage publishing

**Modified Files:**
- `crates/kernel/src/lib.rs` - added stage module export
- `crates/kernel/src/main.rs` - added stage module declaration
- `crates/kernel/src/state.rs` - added StageService to AppState
- `crates/kernel/tests/common/mod.rs` - added stage() accessor

## Change Log

- 2026-02-14: Story created
- 2026-02-14: Implementation complete - 7 tests pass
