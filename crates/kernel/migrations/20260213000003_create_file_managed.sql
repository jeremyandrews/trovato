-- File management table
-- Story 11.1: File Managed Table

CREATE TABLE file_managed (
    id UUID PRIMARY KEY,
    -- User who uploaded the file
    owner_id UUID NOT NULL REFERENCES users(id),
    -- Original filename from upload
    filename VARCHAR(255) NOT NULL,
    -- Storage URI (e.g., "local://uploads/2026/02/file.jpg" or "s3://bucket/key")
    uri VARCHAR(512) NOT NULL UNIQUE,
    -- MIME type
    filemime VARCHAR(255) NOT NULL,
    -- File size in bytes
    filesize BIGINT NOT NULL,
    -- 0 = temporary (not yet attached to content), 1 = permanent
    status SMALLINT NOT NULL DEFAULT 0,
    -- Timestamps
    created BIGINT NOT NULL,
    changed BIGINT NOT NULL
);

-- Index for finding files by owner
CREATE INDEX idx_file_owner ON file_managed(owner_id);

-- Index for cleanup job (find temporary files)
CREATE INDEX idx_file_status ON file_managed(status);

-- Index for finding old temporary files
CREATE INDEX idx_file_created ON file_managed(created) WHERE status = 0;

-- Comments
COMMENT ON TABLE file_managed IS 'Tracks uploaded files and their storage locations';
COMMENT ON COLUMN file_managed.status IS '0=temporary (pending attachment), 1=permanent (attached to content)';
COMMENT ON COLUMN file_managed.uri IS 'Storage URI: local://path or s3://bucket/key';
