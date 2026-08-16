-- P11c / D-44: additive cost-accounting column on ai_usage_log.
-- cost_estimate is the derived cost of the call (input+output tokens priced by
-- the config-owned ai_pricing table), in the model's configured currency.
-- NULL for unpriced models — tokens-only, never an invented cost.
-- DOUBLE PRECISION (not NUMERIC): this is an estimate derived from token counts,
-- and the kernel's sqlx build has no arbitrary-precision decimal decoder.

ALTER TABLE ai_usage_log ADD COLUMN IF NOT EXISTS cost_estimate DOUBLE PRECISION;
