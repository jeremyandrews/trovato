# Story 6.1: Role Definitions & Permission Mapping via Config Import

Status: ready-for-dev

## Story

As a site administrator,
I want to define roles and their permissions via YAML config files,
So that access control is reproducible and version-controlled.

## Acceptance Criteria

1. Five roles imported: anonymous, authenticated, editor, publisher, admin
2. Permission-to-role mappings applied (editor gets "edit any conference", etc.)
3. Re-importing is idempotent

## Tasks / Subtasks

- [ ] Create `role.editor.yml` with permissions (AC: #1, #2)
- [ ] Create `role.publisher.yml` with permissions (AC: #1, #2)
- [ ] Import config and verify roles (AC: #1)
- [ ] Re-import and verify idempotency (AC: #3)

## Dev Notes

- Roles: `crates/kernel/src/models/role.rs`
- Permissions: `crates/kernel/src/permissions.rs`
- anonymous/authenticated/admin are well-known, may already exist
- Config import for roles — verify support in yaml.rs

### References

- [Source: docs/tutorial/plan-parts-03-04.md#Part 4 Step 2]
