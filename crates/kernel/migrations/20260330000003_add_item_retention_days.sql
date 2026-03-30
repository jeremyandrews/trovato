-- Add retention_days to items and revisions for automated data lifecycle management.
-- NULL means no automatic retention policy (content kept indefinitely).
-- A data retention plugin queries these to find expired content.

ALTER TABLE item ADD COLUMN retention_days INTEGER;
ALTER TABLE item_revision ADD COLUMN retention_days INTEGER;
