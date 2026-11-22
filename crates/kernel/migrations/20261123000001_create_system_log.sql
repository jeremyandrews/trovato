-- The site log (B107): warnings and errors the kernel and its plugins emit
-- through `tracing`, plus a short allowlist of notable info events (sign ins,
-- plugin enable and disable, config import), for `/admin/reports/log`.
--
-- Written by `system_log::SystemLogLayer` through a background writer. Never a
-- password, a secret, a session id or a request body: the layer drops fields
-- named for any of them before a row is built. Cron keeps it to the smaller of
-- `SYSTEM_LOG_RETENTION_DAYS` (30) and `SYSTEM_LOG_MAX_ROWS` (100,000).

CREATE TABLE system_log (
    id BIGSERIAL PRIMARY KEY,
    -- Unix seconds.
    created BIGINT NOT NULL,
    -- `error`, `warn` or `info`.
    level VARCHAR(8) NOT NULL,
    -- What kind of event: the `log_type` field when the event names one
    -- (`user`, `plugin`, `config`), otherwise the module that emitted it.
    log_type VARCHAR(128) NOT NULL,
    message TEXT NOT NULL,
    -- The event's other fields, with secret ones removed.
    context JSONB NOT NULL DEFAULT '{}'::jsonb
);

CREATE INDEX idx_system_log_created ON system_log (created);
CREATE INDEX idx_system_log_level_type ON system_log (level, log_type);
