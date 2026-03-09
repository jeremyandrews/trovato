# Story 1.4: File Lifecycle Management

Status: ready-for-dev

## Story

As a system administrator,
I want uploaded files to transition from temporary to permanent on item save and orphaned temps to be cleaned up,
So that disk space is not wasted by abandoned uploads.

## Acceptance Criteria

1. File status transitions from temporary (status=0) to permanent (status=1) on item save
2. File is linked to the saved item after promotion
3. Orphaned temporary files older than 6 hours are deleted by cleanup cron
4. Cleanup removes both storage file and database record

## Tasks / Subtasks

- [ ] Verify/wire temp→permanent file promotion in item save flow (AC: #1, #2)
  - [ ] On item save, find referenced file IDs in JSONB fields
  - [ ] Update `file_managed.status` from 0 to 1 for referenced files
- [ ] Implement/verify cron task for orphaned temp cleanup (AC: #3, #4)
  - [ ] Query `file_managed` for status=0 AND created < (now - 6h)
  - [ ] Delete storage files and database records
- [ ] Add integration test for file lifecycle

## Dev Notes

### Architecture

- File lifecycle: upload creates temp (status=0) → item save promotes to permanent (status=1)
- Cron infrastructure: `crates/kernel/src/cron/` — kernel cron tasks
- `tap_cron` available for plugin-driven cron, but file cleanup is kernel behavior
- `file_managed` table: id, filename, uri, filemime, filesize, status, created, changed
- Cron cleanup should use `file/service.rs` methods, not raw SQL

### Key Files

- `crates/kernel/src/file/service.rs` — FileService with promote/cleanup methods
- `crates/kernel/src/content/item_service.rs` — item save flow where promotion happens
- `crates/kernel/src/cron/` — cron task registration

### References

- [Source: docs/design/Design-Infrastructure.md] — file lifecycle design
- [Source: docs/tutorial/plan-parts-03-04.md#Step 2] — file lifecycle tutorial step
