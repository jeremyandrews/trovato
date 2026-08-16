-- Plugin queue table for tap_queue_worker dispatch.
--
-- Plugins push jobs via the queue_push host function during tap_cron.
-- The kernel cron task pops unprocessed jobs and dispatches tap_queue_worker
-- on the owning plugin.

CREATE TABLE plugin_queue (
    id BIGSERIAL PRIMARY KEY,

    -- Plugin that owns this job (used for tap_queue_worker dispatch).
    plugin_name VARCHAR(64) NOT NULL,

    -- Logical queue name declared in tap_queue_info.
    queue_name VARCHAR(64) NOT NULL,

    -- Job payload (arbitrary JSON, plugin-defined).
    payload JSONB NOT NULL,

    -- Unix timestamp when job was enqueued.
    created_at BIGINT NOT NULL
);

-- Index for efficient per-plugin queue draining.
CREATE INDEX idx_plugin_queue_plugin_created ON plugin_queue (plugin_name, created_at);
