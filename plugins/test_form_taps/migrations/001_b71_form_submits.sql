-- B71 fixture: one row per tap_form_submit the kernel dispatched, so a test
-- can count them.
CREATE TABLE IF NOT EXISTS b71_form_submits (
    id      BIGSERIAL PRIMARY KEY,
    form_id TEXT NOT NULL,
    item_id TEXT NOT NULL DEFAULT ''
);
