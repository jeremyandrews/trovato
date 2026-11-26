-- BG18: durable mail (docs/design/mail-queue.md).
--
-- Every message is a row here, written with the same executor as the event
-- that caused it, and ends sent, failed, suppressed or cancelled. Expand only:
-- two new tables, nothing an older release reads.

CREATE TABLE mail_outbox (
    id BIGSERIAL PRIMARY KEY,
    tenant_id UUID NOT NULL REFERENCES tenant(id) ON DELETE CASCADE,
    kind TEXT NOT NULL CHECK (kind IN ('transactional', 'bulk')),
    -- 0 account mail, 1 notifications, 2 bulk. Lower is sent first.
    priority SMALLINT NOT NULL DEFAULT 1,
    category TEXT NOT NULL,
    recipient TEXT NOT NULL,
    subject TEXT NOT NULL,
    -- Cleared by retention once the message is finished and old enough.
    text_body TEXT,
    html_body TEXT,
    -- [{filename, content_type, bytes_base64}], cleared with the bodies.
    attachments JSONB,
    unsubscribe_url TEXT,
    -- A reference to a one time secret the worker mints at send time, never
    -- the secret itself.
    secret JSONB,
    dedupe_key TEXT,
    status TEXT NOT NULL DEFAULT 'queued'
        CHECK (status IN ('queued', 'sending', 'sent', 'failed', 'suppressed', 'cancelled')),
    attempts INTEGER NOT NULL DEFAULT 0,
    max_attempts INTEGER NOT NULL DEFAULT 12,
    next_attempt_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    locked_until TIMESTAMPTZ,
    locked_by TEXT,
    last_error TEXT,
    held_reason TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    sent_at TIMESTAMPTZ,
    finished_at TIMESTAMPTZ,
    bounced_at TIMESTAMPTZ,
    complained_at TIMESTAMPTZ,
    body_purged_at TIMESTAMPTZ
);

-- A second insert with the same key for the same blog is a no-op.
CREATE UNIQUE INDEX mail_outbox_dedupe
    ON mail_outbox (tenant_id, dedupe_key) WHERE dedupe_key IS NOT NULL;

-- The worker's claim: due rows in priority order.
CREATE INDEX mail_outbox_due
    ON mail_outbox (priority, next_attempt_at, id) WHERE status IN ('queued', 'sending');

-- Per blog limits count what was sent in the last day.
CREATE INDEX mail_outbox_tenant_sent
    ON mail_outbox (tenant_id, kind, sent_at) WHERE sent_at IS NOT NULL;
-- ... and the network bulk limit, what was sent in the last hour.
CREATE INDEX mail_outbox_bulk_sent
    ON mail_outbox (sent_at) WHERE sent_at IS NOT NULL AND kind = 'bulk';

-- Retention: finished rows, oldest first; and those whose body is still held,
-- so the body purge does not walk weeks of rows it already cleared.
CREATE INDEX mail_outbox_finished ON mail_outbox (finished_at) WHERE finished_at IS NOT NULL;
CREATE INDEX mail_outbox_unpurged ON mail_outbox (finished_at)
    WHERE finished_at IS NOT NULL AND body_purged_at IS NULL;

-- The admin list, newest first (by id, which follows insertion), one blog at a
-- time; the network-wide list uses the primary key. It also serves the
-- cascade when a blog is deleted.
CREATE INDEX mail_outbox_tenant_newest ON mail_outbox (tenant_id, id DESC);

-- Addresses no blog mails, network wide: hard bounces (all mail), complaints
-- (bulk mail) and addresses an administrator added by hand.
CREATE TABLE mail_suppression (
    address TEXT PRIMARY KEY CHECK (address = lower(address)),
    reason TEXT NOT NULL CHECK (reason IN ('hard_bounce', 'complaint', 'manual')),
    outbox_id BIGINT REFERENCES mail_outbox(id) ON DELETE SET NULL,
    detail TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Retention deletes outbox rows; this keeps the ON DELETE SET NULL above from
-- scanning the whole list once per deleted row.
CREATE INDEX mail_suppression_outbox ON mail_suppression (outbox_id);
