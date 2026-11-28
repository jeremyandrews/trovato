-- BL-109 reversed: S3-compatible storage and private files.
--
-- Public/private is a scheme independent of which physical backend (local or
-- S3) stores the bytes; that stays in the `uri` prefix (`local://`, `s3://`).
-- 0 = public (served by /files/{path} and the image style route, same as
-- every file before this column existed), 1 = private (served only by
-- /file/{id}/download, after an access check; never reachable at a
-- guessable path).
ALTER TABLE file_managed ADD COLUMN visibility SMALLINT NOT NULL DEFAULT 0;

COMMENT ON COLUMN file_managed.visibility IS '0=public (served by /files/{path}), 1=private (served only by /file/{id}/download)';

-- The private-download route looks up by id and then needs this column; the
-- general listing/admin queries already use the primary key or owner_id/status
-- indexes and do not need visibility indexed on its own.
CREATE INDEX idx_file_visibility ON file_managed(visibility) WHERE visibility = 1;
