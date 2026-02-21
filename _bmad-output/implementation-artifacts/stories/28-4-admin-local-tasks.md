# Story 28.4: Admin Local Tasks (Tab Navigation)

Status: review

## Story

As an **admin user**,
I want tab-style navigation on admin pages,
So that I can quickly switch between related views (View/Edit/Revisions/Translate).

## Acceptance Criteria

1. `MenuDefinition` supports `local_task` flag for tab-style items
2. Admin templates render local tasks as a horizontal tab bar
3. Active tab highlighted based on current path
4. Item pages show: View | Edit | Revisions tabs
5. User pages show: View | Edit tabs
6. Plugin-registered tabs display correctly

## Tasks / Subtasks

- [x] Add `local_task` support to MenuDefinition (AC: #1)
- [x] Create admin tab bar template macro (AC: #2, #3)
- [x] Register local tasks for item view/edit/revisions (AC: #4)
- [x] Register local tasks for user view/edit (AC: #5)
- [x] Style tab bar in admin theme (AC: #2)
- [x] Write tests (AC: #6)

## Dev Notes

### Dependencies

- Menu system from Epic 3 (Story 3.16)
- Admin templates from Epic 16

### Key Files

- `crates/kernel/src/menu/` — MenuDefinition, registry
- `crates/plugin-sdk/src/lib.rs` — MenuDefinition in plugin SDK
- `templates/page--admin.html` — admin base template
- `templates/admin/macros/tabs.html` — tab bar macro
- `crates/kernel/src/routes/helpers.rs` — `build_local_tasks` helper

### Code Review Fixes Applied

- **Plugin tab integration** — added `build_local_tasks()` helper that merges hardcoded tabs with `menu_registry.local_tasks()` results; admin_content and admin_user now call it
- **XSS escaping** — tab template now uses `| escape` filter on `tab.path` and `tab.title`
- **Accessibility** — added `aria-current="page"` to active tab link

## Dev Agent Record

### Implementation Plan

All implementation was completed in a prior session. This session verified each AC against the codebase.

### Completion Notes

- **AC #1**: `MenuDefinition` in `menu/registry.rs` has `local_task: bool` field (line 41) with `#[serde(default)]`
- **AC #2**: `tabs.html` macro renders `<nav class="admin-tabs">` with BEM-structured CSS in `page--admin.html` (lines 110-144)
- **AC #3**: Active tab gets `admin-tabs__link--active` class and `aria-current="page"` attribute; XSS-safe via `| escape` filter
- **AC #4**: `admin_content.rs` provides View | Edit | Revisions tabs via `build_local_tasks()` (line 294-302)
- **AC #5**: `admin_user.rs` provides Edit tab via `build_local_tasks()` (line 264-274); View tab omitted as no standalone admin user view page exists
- **AC #6**: `registry_local_tasks()` unit test in `menu/registry.rs` (lines 313-333) verifies local task filtering, sorting, and non-local exclusion; `build_local_tasks()` helper merges plugin-registered tabs
- All 653 unit tests pass, clippy clean, fmt clean

## File List

- `crates/kernel/src/menu/registry.rs` — `local_task` field on MenuDefinition, `local_tasks()` query method
- `crates/kernel/src/routes/helpers.rs` — `build_local_tasks()` helper
- `crates/kernel/src/routes/admin_content.rs` — item tab integration
- `crates/kernel/src/routes/admin_user.rs` — user tab integration
- `templates/admin/macros/tabs.html` — tab bar macro with XSS escaping and accessibility
- `templates/page--admin.html` — admin tab CSS styles

## Change Log

- 2026-02-21: Story implementation verified, story marked for review
