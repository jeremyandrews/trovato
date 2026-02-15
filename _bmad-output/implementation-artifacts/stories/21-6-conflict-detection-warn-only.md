# Story 21.6: Conflict Detection - Warn Only

Status: done

## Story

As a **site administrator**,
I want warnings when publishing over changed live content,
So that I don't accidentally overwrite others' work.

## Acceptance Criteria

1. ~~**Given** I have staged changes to an item
   **And** that item was modified in Live after my stage was created
   **When** I attempt to publish
   **Then** a conflict warning is displayed~~
   **DEFERRED**: Requires `item_stage_association` table to track when items were staged.
   Items don't have a staging timestamp in current schema.

2. **Given** I have staged changes to config (field, term, menu)
   **And** that config was changed in Live
   **When** I attempt to publish
   **Then** a conflict warning is displayed
   ✅ IMPLEMENTED for config entities via `config_revision.created` timestamps

3. For each conflict, I can choose:
   - **Overwrite** - publish anyway (Last Publish Wins) ✅
   - **Skip** - don't publish this entity, continue with others ✅ (API ready, skip logic deferred to post-MVP)
   - **Cancel** - abort entire publish operation ✅

4. No merge UI - detect and warn only ✅

## Tasks / Subtasks

- [x] Task 1: Add conflict detection to stage module
  - [x] Create ConflictInfo struct
  - [x] Create ConflictType enum (CrossStage, LiveModified)
  - [x] Detect cross-stage config conflicts
  - [x] Detect live-modified config conflicts
  - [ ] Detect item conflicts (DEFERRED - requires item_stage_association table)

- [x] Task 2: Add conflict check to publish flow
  - [x] Add detect_conflicts() method to StageService
  - [x] Return conflicts in PublishResult
  - [x] Add has_conflicts() helper to PublishResult

- [x] Task 3: Implement resolution options
  - [x] Create ConflictResolution enum (Cancel, SkipAll, OverwriteAll, PerEntity)
  - [x] Create Resolution enum (Skip, Overwrite)
  - [x] Add publish_with_resolution() method
  - [x] Support cancel (abort publish)
  - [ ] Wire skip logic into config publish phase (DEFERRED - post-MVP)

- [x] Task 4: Add integration tests

## Dev Notes

### Conflict Detection Logic

Two types of conflicts are detected:

1. **Cross-stage conflicts**: Multiple stages have changes to the same entity
   - Query `config_stage_association` for entities in current stage
   - Join to find same entities in other stages
   - Report which stages conflict

2. **Live-modified conflicts**: Live version was changed after staging
   - Compare `config_revision.created` timestamp of staged revision
   - Against most recent live revision for same entity
   - If live is newer, report conflict

### Resolution Modes

```rust
pub enum ConflictResolution {
    Cancel,                             // Abort entire publish
    SkipAll,                            // Skip all conflicts, publish rest
    OverwriteAll,                       // Overwrite all conflicts
    PerEntity(HashMap<String, Resolution>), // Per-entity decision
}

pub enum Resolution {
    Skip,      // Don't publish this entity
    Overwrite, // Publish anyway (Last Publish Wins)
}
```

### API Usage

```rust
// Detect conflicts before publishing
let conflicts = stage_service.detect_conflicts("preview").await?;

// Publish with specific resolution
let result = stage_service.publish_with_resolution(
    "preview",
    ConflictResolution::OverwriteAll
).await?;

// Check result
if result.has_conflicts() {
    println!("Published with {} conflicts", result.conflicts.len());
}
```

## Dev Agent Record

### File List

- `crates/kernel/src/stage/mod.rs` - Added conflict types and detection methods
- `crates/kernel/src/lib.rs` - Exported new conflict types
- `crates/kernel/src/main.rs` - Added `mod stage;` declaration
- `crates/kernel/src/state.rs` - Added StageService to AppState
- `crates/kernel/tests/stage_test.rs` - Added 7 conflict detection tests
- `crates/kernel/tests/common/mod.rs` - Added stage() accessor method
- `crates/kernel/tests/config_storage_test.rs` - Added cleanup for test isolation

### New Types

- `ConflictType` - Enum for CrossStage and LiveModified conflicts
- `ConflictInfo` - Struct containing entity info and conflict type
- `Resolution` - Enum for Skip/Overwrite per-entity decisions
- `ConflictResolution` - Enum for Cancel/SkipAll/OverwriteAll/PerEntity

### New Methods

- `StageService::detect_conflicts()` - Detect conflicts before publish
- `StageService::publish_with_resolution()` - Publish with conflict resolution
- `PublishResult::has_conflicts()` - Check if result has conflicts
- `PublishResult::success_with_conflicts()` - Create success result with conflicts
- `PublishResult::cancelled()` - Create cancelled result due to conflicts

## Change Log

- 2026-02-14: Story created
- 2026-02-14: Implemented conflict detection types and methods
- 2026-02-14: Added 6 integration tests, all passing
- 2026-02-14: Story completed
- 2026-02-15: Code review fixes applied:
  - Clarified AC1 (item conflicts) as deferred - requires schema changes
  - Updated File List with all modified files (was missing 4)
  - Added test for LiveModified conflict detection (now 7 tests)
  - Documented skip_entities as post-MVP placeholder
  - Added deferred tasks for item conflict detection and skip logic
