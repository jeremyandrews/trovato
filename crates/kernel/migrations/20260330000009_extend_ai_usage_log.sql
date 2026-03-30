-- Extend AI usage log with audit trail metadata (Story 47.4).
-- finish_reason: how the AI response ended (stop, length, error)
-- status: success, error, timeout, denied
-- deny_reason: why the request was denied (from tap_ai_request)

ALTER TABLE ai_usage_log ADD COLUMN IF NOT EXISTS finish_reason VARCHAR(64);
ALTER TABLE ai_usage_log ADD COLUMN IF NOT EXISTS status VARCHAR(32) DEFAULT 'success';
ALTER TABLE ai_usage_log ADD COLUMN IF NOT EXISTS deny_reason TEXT;
