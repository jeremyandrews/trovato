-- AI usage log for token budget tracking.
-- Records every ai_request() host function call with token counts,
-- provider/model info, and timing for budget enforcement and dashboards.

CREATE TABLE IF NOT EXISTS ai_usage_log (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID,
    plugin_name VARCHAR(255) NOT NULL,
    provider_id VARCHAR(255) NOT NULL,
    operation VARCHAR(64) NOT NULL,
    model VARCHAR(255) NOT NULL,
    prompt_tokens INTEGER NOT NULL DEFAULT 0,
    completion_tokens INTEGER NOT NULL DEFAULT 0,
    total_tokens INTEGER NOT NULL DEFAULT 0,
    latency_ms BIGINT NOT NULL DEFAULT 0,
    created BIGINT NOT NULL DEFAULT EXTRACT(EPOCH FROM NOW())::bigint
);

CREATE INDEX IF NOT EXISTS idx_ai_usage_log_created ON ai_usage_log (created);
CREATE INDEX IF NOT EXISTS idx_ai_usage_log_user_id ON ai_usage_log (user_id);
CREATE INDEX IF NOT EXISTS idx_ai_usage_log_provider ON ai_usage_log (provider_id);
CREATE INDEX IF NOT EXISTS idx_ai_usage_log_budget ON ai_usage_log (provider_id, created);
