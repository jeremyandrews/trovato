-- Form state cache for AJAX and multi-step forms
-- Stores serialized form state keyed by form_build_id

CREATE TABLE form_state_cache (
    form_build_id VARCHAR(64) PRIMARY KEY,
    form_id VARCHAR(128) NOT NULL,
    state JSONB NOT NULL,
    created BIGINT NOT NULL,
    updated BIGINT NOT NULL
);

CREATE INDEX idx_form_state_updated ON form_state_cache(updated);
-- Cleanup job deletes entries older than 6 hours
