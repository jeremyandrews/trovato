-- P11g / D-59 reference plugin: the lightweight-record backing table.
--
-- An item-like shape declared as a lightweight record type in
-- `trovato_record_ref.info.toml` — a UUID primary key, a title, created/changed
-- timestamps, an author, a boolean published flag, and three mapped fields
-- (`location`, `capacity`, `secret_notes`). The kernel serves gather, admin
-- listing/view, RecordReference, and the FR-8 field-access seam over it; the
-- plugin owns writes. `secret_notes` is the field the plugin's tap_field_access
-- governs (deny without the "view secret_notes" permission).
CREATE TABLE IF NOT EXISTS record_event (
    id           UUID PRIMARY KEY,
    title        VARCHAR(255) NOT NULL,
    author_id    UUID,
    published    BOOLEAN NOT NULL DEFAULT true,
    location     TEXT,
    capacity     INTEGER,
    secret_notes TEXT,
    created      BIGINT NOT NULL,
    changed      BIGINT NOT NULL
);
