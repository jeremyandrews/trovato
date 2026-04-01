# Story 39.2: Batch Operations Service

Status: done

## Story

As a **site administrator**,
I want to run long-running operations (reindex, pathauto regeneration, bulk publish) as tracked background jobs,
so that I can monitor progress and the UI remains responsive during large operations.

## Acceptance Criteria

1. `BatchService` manages batch operations stored in Redis with 24-hour TTL
2. Batch operation types supported: reindex, pathauto regenerate, stage publish, config import
3. Progress tracking with `BatchStatus` states: Pending, Running, Complete, Failed, Cancelled
4. `BatchProgress` tracks total items, processed count, current operation label, and percentage
5. Operations are created with `CreateBatch` (operation type + JSON params), returning a `BatchOperation` with UUID
6. Operations can be retrieved by ID and updated with progress or completion/failure status
7. Operation listing supported for status monitoring

## Tasks / Subtasks

- [x] Define `BatchOperation` struct with id, type, status, progress, params, result, error, timestamps (AC: #1, #5)
- [x] Define `BatchStatus` enum with Pending/Running/Complete/Failed/Cancelled variants (AC: #3)
- [x] Define `BatchProgress` struct with total/processed/current_operation/percentage fields (AC: #4)
- [x] Implement `BatchProgress::new()`, `update()`, and `complete()` helpers (AC: #4)
- [x] Define `CreateBatch` request type with operation_type and params (AC: #5)
- [x] Implement `BatchService` backed by Redis with `batch:` key prefix and 24h TTL (AC: #1)
- [x] Implement `create()` generating UUIDv7 and storing serialized operation (AC: #5)
- [x] Implement `get()` for retrieving operations by ID (AC: #6)
- [x] Implement `update_progress()` and `complete()`/`fail()` status transitions (AC: #6)
- [x] Implement `list()` for operation enumeration (AC: #7)

## Dev Notes

### Architecture

The batch service is Redis-backed rather than Postgres-backed, since batch operations are ephemeral (24h TTL) and benefit from Redis's fast read/write for progress polling. Each operation is stored as a serialized JSON blob at `batch:{uuid}` with automatic expiration. UUIDv7 is used for time-ordered IDs.

The `BatchProgress` struct provides a simple progress model: percentage is auto-calculated from processed/total. The `current_operation` field allows UI display of what step is currently executing (e.g., "Reindexing item 450 of 5000").

### Testing

- Integration tests in `crates/kernel/tests/` exercise create/get/update/complete/fail flows
- Tests require a running Redis instance

### References

- `crates/kernel/src/batch/mod.rs` (11 lines) -- module re-exports
- `crates/kernel/src/batch/types.rs` (116 lines) -- BatchOperation, BatchStatus, BatchProgress, CreateBatch
- `crates/kernel/src/batch/service.rs` (222 lines) -- BatchService implementation
- `crates/kernel/src/routes/batch.rs` -- admin route handler for batch operations
