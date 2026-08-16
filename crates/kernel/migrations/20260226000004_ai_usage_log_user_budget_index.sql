-- Composite index for budget check queries: WHERE user_id = $1 AND provider_id = $2 AND created >= $3
CREATE INDEX IF NOT EXISTS idx_ai_usage_log_user_budget ON ai_usage_log (user_id, provider_id, created);
