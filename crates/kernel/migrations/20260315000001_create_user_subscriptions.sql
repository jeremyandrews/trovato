-- Create user_subscriptions table for item subscription tracking.
-- Forward-only migration; no rollback.

CREATE TABLE IF NOT EXISTS user_subscriptions (
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    item_id UUID NOT NULL REFERENCES item(id) ON DELETE CASCADE,
    created BIGINT NOT NULL DEFAULT EXTRACT(EPOCH FROM NOW())::bigint,
    PRIMARY KEY (user_id, item_id)
);

CREATE INDEX IF NOT EXISTS idx_user_subscriptions_item ON user_subscriptions(item_id);
