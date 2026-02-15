# Story 21.5: Stage-Aware Menus & Aliases

Status: blocked

## Story

As a **site administrator**,
I want menus and URL aliases to be stage-aware,
So that navigation and URLs work correctly in stage preview.

## Acceptance Criteria

1. **Given** I create a menu item linking to staged content
   **When** I preview the stage
   **Then** the menu item appears and links work
   **And** Live menus don't show the item

2. **Given** I create a URL alias for staged content
   **When** I access that alias in stage preview
   **Then** the alias resolves to the staged item
   **And** the alias doesn't exist in Live

3. **Given** two stages create the same alias for different items
   **When** I try to create the second alias
   **Then** a conflict warning is shown

4. **Given** I publish a stage with menu/alias changes
   **When** publishing completes
   **Then** menus and aliases are live

## Blockers

### Menu System Architecture

Menus are currently plugin-driven, not database-driven:
- Menus are defined via `tap_menu` tap from plugins
- MenuRegistry is built at startup from plugin results
- No database table for menu items

To stage menus, we would need to:
1. Create `menu` and `menu_link` tables
2. Add Menu and MenuLink to ConfigEntity
3. Merge plugin menus with database menus
4. Wire through StageAwareConfigStorage

### URL Aliases Not Implemented

Story 15.5 (Path Alias System) is in backlog:
- No `url_alias` table exists
- No path resolution middleware
- Required for stage-aware alias support

## Tasks / Subtasks

- [ ] Task 1: Add menu database storage (blocked on architecture decision)
  - [ ] Create menu table migration
  - [ ] Add Menu to ConfigEntity
  - [ ] Merge plugin and database menus

- [ ] Task 2: Wire menu through StageAwareConfigStorage
  - [ ] Requires Task 1

- [ ] Task 3: Implement Story 15.5 (URL aliases)
  - [ ] Create url_alias table
  - [ ] Add UrlAlias to ConfigEntity
  - [ ] Implement path resolution middleware

- [ ] Task 4: Wire URL aliases through StageAwareConfigStorage
  - [ ] Requires Task 3

- [ ] Task 5: Add conflict detection for aliases
  - [ ] Detect same alias in multiple stages

## Dev Notes

This story depends on:
- Menu database storage (not yet implemented)
- Story 15.5: Path Alias System (backlog)

The StageAwareConfigStorage infrastructure is ready to support menus and aliases
once they are added to ConfigEntity.

## Dev Agent Record

### File List

No files - blocked.

## Change Log

- 2026-02-14: Story created
- 2026-02-14: Marked blocked - depends on menu DB storage and Story 15.5
