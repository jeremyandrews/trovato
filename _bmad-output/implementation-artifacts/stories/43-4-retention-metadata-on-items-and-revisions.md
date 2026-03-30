# Story 43.4: Retention Metadata on Items and Revisions

Status: ready-for-dev

## Story

As a **data retention plugin developer**,
I want retention metadata on items and revisions,
so that I can implement automated data cleanup policies.

## Acceptance Criteria

1. Migration adds `retention_days` (INTEGER, NULLABLE) to `item` table
2. Migration adds `retention_days` (INTEGER, NULLABLE) to `item_revision` table
3. When `retention_days` is NULL, no automatic retention policy applies (content kept indefinitely -- the default)
4. Admin UI item edit form includes optional "Retention period (days)" field
5. Content type definition can specify a default `retention_days` for new items of that type
6. `Item` model includes `retention_days` field, serialized to plugins
7. Kernel does NOT implement the retention cron job -- only the schema. The cron job is plugin territory.
8. At least 1 integration test: create item with retention_days, verify it persists

## Tasks / Subtasks

- [ ] Write migration SQL adding `retention_days` to `item` table (AC: #1)
- [ ] Write migration SQL adding `retention_days` to `item_revision` table (AC: #2)
- [ ] Add `retention_days: Option<i32>` field to `Item` model struct (AC: #6)
- [ ] Update item query methods to select and populate `retention_days` (AC: #6)
- [ ] Update item create/update logic to persist `retention_days` (AC: #6)
- [ ] Add optional "Retention period (days)" field to admin item edit form template (AC: #4)
- [ ] Update item form route handler to read/write `retention_days` (AC: #4)
- [ ] Add `default_retention_days: Option<i32>` to content type definition (AC: #5)
- [ ] Apply content type default when creating new items without explicit retention_days (AC: #5)
- [ ] Write integration test: create item with retention_days, reload, verify value persists (AC: #8)

## Dev Notes

### Architecture

- Two separate migrations: one for `item`, one for `item_revision`. Both are `ALTER TABLE ... ADD COLUMN retention_days INTEGER`.
- NULL means "keep indefinitely" -- this is the default for all existing content and new content without explicit retention.
- The kernel stores the metadata. A retention plugin runs a cron job (`tap_cron`) that queries `WHERE retention_days IS NOT NULL AND created + retention_days * interval '1 day' < NOW()` and deletes/archives expired content.
- The audit log service already has `cleanup(retention_days)` in `services/audit.rs` -- the retention plugin can use a similar pattern.

**Relationship to `users.data_retention_days` (Story 43.1):** These are independent concepts with different scopes:
- `users.data_retention_days` — how long to retain the *user account and profile data* after the user requests deletion or becomes inactive. This is about user lifecycle.
- `item.retention_days` — how long to retain the *content* regardless of the author's status. This is about content lifecycle.
A user might have `data_retention_days = 90` (delete my account after 90 days of inactivity) while their authored items have `retention_days = 365` (keep this content for a year). There is no precedence conflict — they govern different retention targets. A retention plugin should handle both independently.

### Security

- Only users with item edit permissions can set `retention_days` on items.
- The kernel intentionally does NOT implement automatic deletion -- that is plugin territory, allowing site operators to choose their retention policy implementation.

### Testing

- Integration test: create an item via the API or service with `retention_days = 30`, reload it, assert the value is 30.
- Verify NULL default: create an item without specifying retention_days, assert it is NULL.
- Verify content type default: set `default_retention_days` on a content type, create a new item, assert it inherits the default.

### References

- [Source: docs/ritrovo/epic-13-privacy.md -- Story 43.4]
- [Source: crates/kernel/src/models/item.rs -- Item model]
- [Source: crates/kernel/src/services/audit.rs -- cleanup(retention_days) pattern]
