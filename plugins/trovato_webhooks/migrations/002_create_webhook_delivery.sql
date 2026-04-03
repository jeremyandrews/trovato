-- Create webhook_delivery table for delivery tracking and retry.
-- Forward-only migration; no rollback.

CREATE TABLE IF NOT EXISTS webhook_delivery (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    webhook_id UUID NOT NULL REFERENCES webhook(id) ON DELETE CASCADE,
    event VARCHAR(255) NOT NULL,
    payload JSONB NOT NULL DEFAULT '{}',
    status_code SMALLINT,
    response TEXT,
    attempts SMALLINT NOT NULL DEFAULT 0,
    next_retry BIGINT,
    created BIGINT NOT NULL DEFAULT EXTRACT(EPOCH FROM NOW())::bigint
);

CREATE INDEX IF NOT EXISTS idx_webhook_delivery_webhook ON webhook_delivery (webhook_id);
CREATE INDEX IF NOT EXISTS idx_webhook_delivery_next_retry ON webhook_delivery (next_retry)
    WHERE next_retry IS NOT NULL;
