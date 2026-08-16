-- Plugin queue v2 (P11d / D-45): evolve `plugin_queue` from a plain
-- table + sequential drain into a real work queue with attempts, backoff,
-- priority, claim-locking, terminal status, and a dead-letter tier.
--
-- ADDITIVE ONLY. Every new column carries a default, so existing in-flight
-- rows (e.g. ritrovo_importer jobs enqueued under v1) survive the migration
-- as ready-to-run: `attempts = 0`, `priority = 0`, `status = 'ready'`,
-- `next_attempt_at = 0` (0 means "eligible now"). No data is lost and the
-- `queue_push` host ABI is unchanged (D-48).
--
-- Semantics enabled here become 1.0 contract via D-47 (concurrency +
-- at-least-once delivery); the schema itself is kernel-internal (not frozen).

-- Number of dispatch attempts already made for this job. Incremented by the
-- drain each time `tap_queue_worker` fails (traps or returns an error result).
ALTER TABLE plugin_queue
    ADD COLUMN attempts INT NOT NULL DEFAULT 0;

-- Maximum attempts before the job is dead-lettered. When `attempts` reaches
-- this bound the job moves to `status = 'dead'` instead of being retried,
-- so a poison item can never block its queue or retry forever.
ALTER TABLE plugin_queue
    ADD COLUMN max_attempts INT NOT NULL DEFAULT 5;

-- Earliest Unix timestamp (seconds) at which the job may be claimed again.
-- Set to `now + backoff` after a failed attempt (exponential backoff) and to
-- `now + delay` for delayed enqueues (D-48 `enqueue` opts). 0 = eligible now.
ALTER TABLE plugin_queue
    ADD COLUMN next_attempt_at BIGINT NOT NULL DEFAULT 0;

-- Dispatch priority. Higher values are claimed first; ties break by
-- `created_at` (FIFO). Default 0 keeps v1 rows in insertion order.
ALTER TABLE plugin_queue
    ADD COLUMN priority INT NOT NULL DEFAULT 0;

-- Claim lease expiry (Unix seconds). While a worker holds a job the row is
-- `status = 'claimed'` with `locked_until = now + lease`. If a claimer dies,
-- the lease expires and the job is reclaimable — this is what makes delivery
-- at-least-once (D-47): a crashed worker's job is retried, never lost.
-- 0 = not currently claimed.
ALTER TABLE plugin_queue
    ADD COLUMN locked_until BIGINT NOT NULL DEFAULT 0;

-- Terminal/lifecycle status. `ready` (claimable), `claimed` (leased to a
-- worker), `done` (reserved — successful jobs are deleted, see the drain),
-- `dead` (dead-lettered; retained for inspection).
ALTER TABLE plugin_queue
    ADD COLUMN status TEXT NOT NULL DEFAULT 'ready';

ALTER TABLE plugin_queue
    ADD CONSTRAINT plugin_queue_status_check
    CHECK (status IN ('ready', 'claimed', 'done', 'dead'));

-- Last error observed on a failed attempt (worker trap / error result),
-- preserved across retries so the DLQ shows why the final attempt failed.
ALTER TABLE plugin_queue
    ADD COLUMN last_error TEXT;

-- Human-facing dead-letter reason, set when the job moves to `status = 'dead'`.
ALTER TABLE plugin_queue
    ADD COLUMN dead_reason TEXT;

-- When the job was dead-lettered (Unix seconds), for DLQ ordering/observability.
ALTER TABLE plugin_queue
    ADD COLUMN dead_at BIGINT;

-- Claim-selection index: the drain filters by (plugin_name, status) and orders
-- eligible rows by (priority DESC, created_at ASC). Covers the hot
-- `FOR UPDATE SKIP LOCKED` claim query.
CREATE INDEX idx_plugin_queue_claim
    ON plugin_queue (plugin_name, status, priority DESC, created_at);

-- Partial index for the DLQ admin/inspection surface — only dead rows.
CREATE INDEX idx_plugin_queue_dead
    ON plugin_queue (dead_at)
    WHERE status = 'dead';
