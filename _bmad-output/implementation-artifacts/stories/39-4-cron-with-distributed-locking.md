# Story 39.4: Cron with Distributed Locking

Status: done

## Story

As a **platform operator**,
I want scheduled background tasks to run exactly once across a multi-instance deployment,
so that maintenance operations, queue processing, and plugin cron taps execute reliably without duplication.

## Acceptance Criteria

1. Redis-based distributed lock (`cron:lock` key) with 5-minute TTL prevents concurrent execution across instances
2. Lock heartbeat refreshes TTL every 60 seconds during long-running cron cycles
3. `tap_cron` dispatch invokes plugin-registered cron taps during each cycle
4. Queue worker processes background jobs from `RedisQueue`
5. Kernel maintenance tasks: temporary file cleanup, expired session pruning
6. External trigger via HTTP POST endpoint allows on-demand cron execution
7. Pagefind index rebuild support for search index maintenance
8. `CronResult` enum reports Completed (with task list and duration), Skipped (lock held), or Failed

## Tasks / Subtasks

- [x] Implement `CronService` struct with Redis client, PgPool, and task registry (AC: #1)
- [x] Implement distributed lock acquisition with `SET NX EX` and 5-minute TTL (AC: #1)
- [x] Implement heartbeat task that refreshes lock TTL every 60 seconds (AC: #2)
- [x] Implement `CronTasks` with kernel maintenance: temp file cleanup, session pruning (AC: #5)
- [x] Implement `tap_cron` dispatch via `TapDispatcher` for plugin cron hooks (AC: #3)
- [x] Implement `RedisQueue` for background job queuing and processing (AC: #4)
- [x] Implement queue worker that drains and processes queued jobs during cron cycle (AC: #4)
- [x] Implement pagefind index rebuild integration (AC: #7)
- [x] Define `CronResult` enum with Completed/Skipped/Failed variants (AC: #8)
- [x] Add HTTP POST cron trigger route (AC: #6)
- [x] Add unit tests for cron tasks and queue operations (AC: #1, #4, #5)

## Dev Notes

### Architecture

The cron system uses a leader-election pattern via Redis distributed locks. When a cron cycle is triggered (either by interval timer or HTTP POST), the instance attempts to acquire `cron:lock` with `SET NX EX 300`. If the lock is acquired, the instance becomes the leader for that cycle and runs all tasks. A background heartbeat task refreshes the lock TTL every 60 seconds to prevent expiration during long-running operations.

The task execution order is:
1. Kernel maintenance (temp cleanup, session pruning)
2. Queue worker drains `RedisQueue`
3. `tap_cron` dispatches to all plugin-registered cron taps
4. Pagefind index rebuild (if enabled)
5. AI token budget reset (if AI services configured)

The `CronService` holds optional `Arc` references to `TapDispatcher`, `AiProviderService`, and `AiTokenBudgetService`, following the kernel minimality pattern of `Option<Arc<ServiceType>>` for plugin-optional dependencies.

### Testing

- Unit tests in `crates/kernel/src/cron/mod.rs` (2 tests)
- Unit tests in `crates/kernel/src/cron/queue.rs` (1 test)
- Unit tests in `crates/kernel/src/cron/tasks.rs` (1 test)
- Integration tests in `crates/kernel/tests/cron_test.rs` (4 tests)
- Tests require running Redis and Postgres instances

### References

- `crates/kernel/src/cron/mod.rs` (650 lines) -- CronService, distributed locking, heartbeat, main run loop
- `crates/kernel/src/cron/tasks.rs` (275 lines) -- CronTasks, kernel maintenance operations
- `crates/kernel/src/cron/queue.rs` (130 lines) -- Queue trait, RedisQueue implementation
- `crates/kernel/src/cron/pagefind.rs` (324 lines) -- Pagefind index rebuild integration
