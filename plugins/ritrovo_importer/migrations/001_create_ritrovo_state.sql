-- Persistent key-value state for the ritrovo_importer plugin.
-- Used to track import timestamps, topic offsets, and ETags.

CREATE TABLE ritrovo_state (
    name VARCHAR(255) PRIMARY KEY,
    value TEXT NOT NULL
);
