CREATE TABLE IF NOT EXISTS pagefind_index_status (
    id INTEGER PRIMARY KEY DEFAULT 1 CHECK (id = 1),
    last_indexed_at BIGINT NOT NULL DEFAULT 0,
    rebuild_requested BOOLEAN NOT NULL DEFAULT false,
    last_error TEXT
);
INSERT INTO pagefind_index_status (id) VALUES (1) ON CONFLICT DO NOTHING;
