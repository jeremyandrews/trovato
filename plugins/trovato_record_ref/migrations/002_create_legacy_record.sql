-- A lightweight-record backing table keyed by a BIGINT rather than a UUID.
--
-- The shape a plugin gets when it exposes a table it did not design a key for:
-- an existing, integer-keyed table it wraps as a record type. Nothing about the
-- record tier requires a UUID key — the manifest declares which column is the
-- key, and the kernel's read surfaces compare it as text — so this table is
-- here to hold that open, and the admin list/view routes serve it exactly as
-- they serve `record_event`.
CREATE TABLE IF NOT EXISTS record_legacy (
    id      BIGINT PRIMARY KEY,
    title   VARCHAR(255) NOT NULL,
    created BIGINT NOT NULL,
    changed BIGINT NOT NULL
);
