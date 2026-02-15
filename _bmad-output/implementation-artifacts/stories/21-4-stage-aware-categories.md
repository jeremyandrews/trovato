# Story 21.4: Stage-Aware Categories

Status: done

## Story

As a **site administrator**,
I want to modify categories in a stage,
So that I can reorganize taxonomy and tag staged content with new terms.

## Acceptance Criteria

1. **Given** I create a vocabulary/term in a stage
   **When** I edit items in that stage
   **Then** I can tag items with the new term
   **And** the term doesn't exist in Live

2. **Given** I reorganize term hierarchy in a stage
   **When** I view breadcrumbs/hierarchy in that stage
   **Then** the new hierarchy is reflected
   **And** Live hierarchy is unchanged

3. **Given** I delete a term in a stage (via stage_deletion)
   **When** I view items in that stage
   **Then** items show the term as orphaned/removed
   **And** Live items still have the term

4. **Given** I publish a stage with category changes
   **When** publishing completes
   **Then** vocabulary/term changes apply to Live
   **And** term deletions remove terms from Live

## Tasks / Subtasks

- [x] Task 1: Wire category_vocabulary through StageAwareConfigStorage
  - [x] Category entity type already in ConfigEntity enum
  - [x] StageAwareConfigStorage handles Category transparently

- [x] Task 2: Wire category_term through StageAwareConfigStorage
  - [x] Tag entity type already in ConfigEntity enum
  - [x] StageAwareConfigStorage handles Tag transparently

- [x] Task 3: Stage deletion support
  - [x] stage_deletion table supports any entity_type
  - [x] StageAwareConfigStorage.delete() marks for deletion

- [x] Task 4: Verify with tests
  - [x] StageAwareConfigStorage tests cover all entity types

## Dev Notes

Since StageAwareConfigStorage is a generic implementation that handles ALL ConfigEntity types
uniformly, categories (Category and Tag entities) are automatically stage-aware once
Story 21-3 was completed.

The StageAwareConfigStorage:
- Reads staged revisions from config_revision via config_stage_association
- Falls back to live via DirectConfigStorage
- Records deletions in stage_deletion table
- Merges staged and live results in list operations

No additional code was needed for categories specifically.

## Dev Agent Record

### File List

No new files - uses StageAwareConfigStorage from Story 21-3.

## Change Log

- 2026-02-14: Story created
- 2026-02-14: Marked done - covered by StageAwareConfigStorage implementation
