-- K1 e2e fixture: the plugin-owned table an authenticated caller writes through
-- `tap_api` (G-NO-PLUGIN-HTTP). Not a kernel table; the plugin declares it in
-- `db_tables` and owns every row.
CREATE TABLE IF NOT EXISTS tpa_notes (
    user_id UUID NOT NULL,
    slug    TEXT NOT NULL,
    text    TEXT NOT NULL DEFAULT '',
    method  TEXT NOT NULL DEFAULT '',
    PRIMARY KEY (user_id, slug)
);
