-- Create pending_notifications table for queued notification delivery.
-- Forward-only migration; no rollback.

CREATE TABLE IF NOT EXISTS pending_notifications (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL,
    event_type VARCHAR(64) NOT NULL,
    item_id UUID,
    payload JSONB NOT NULL DEFAULT '{}',
    created BIGINT NOT NULL DEFAULT EXTRACT(EPOCH FROM NOW())::bigint,
    sent BOOLEAN NOT NULL DEFAULT FALSE,
    sent_at BIGINT
);

CREATE INDEX IF NOT EXISTS idx_pending_notifications_user
    ON pending_notifications(user_id, sent);
CREATE INDEX IF NOT EXISTS idx_pending_notifications_unsent
    ON pending_notifications(sent, created)
    WHERE NOT sent;
