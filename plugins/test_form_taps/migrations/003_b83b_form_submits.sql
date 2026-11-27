-- B83b fixture: one row per tap_form_submit the kernel dispatched for a form
-- in B83B_FORMS, apart from b71_form_submits, whose count the B71 tests read.
CREATE TABLE IF NOT EXISTS b83b_form_submits (
    id      BIGSERIAL PRIMARY KEY,
    form_id TEXT NOT NULL
);
