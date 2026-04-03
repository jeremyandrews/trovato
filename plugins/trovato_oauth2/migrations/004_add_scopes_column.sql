-- Add scopes column to oauth_client.
-- When set (non-empty array), restricts which scopes the client may request.
-- An empty array means the client has no scope restrictions (for backward compat).

ALTER TABLE oauth_client
ADD COLUMN IF NOT EXISTS scopes JSONB NOT NULL DEFAULT '[]';
