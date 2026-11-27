-- B83b fixture: a form id listed here is refused by tap_form_validate, so a
-- test can refuse a form whose inputs are passwords, which a plugin never sees.
CREATE TABLE IF NOT EXISTS b83b_refuse_forms (
    form_id TEXT PRIMARY KEY
);
