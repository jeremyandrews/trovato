# Story 28.4: Admin Local Tasks (Tab Navigation)

Status: ready-for-dev

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

- [ ] Add `local_task` support to MenuDefinition (AC: #1)
- [ ] Create admin tab bar template macro (AC: #2, #3)
- [ ] Register local tasks for item view/edit/revisions (AC: #4)
- [ ] Register local tasks for user view/edit (AC: #5)
- [ ] Style tab bar in admin theme (AC: #2)
- [ ] Write tests (AC: #6)

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
