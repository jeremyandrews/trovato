-- Content moderation (B158): the workflow state of each revision.
--
-- A moderated content type's revisions each carry a state, `draft`,
-- `published` or `archived` (`content::moderation`). The item row stays the
-- default revision, the one visitors see, so a draft saved over a published
-- item is a revision only, and nothing on the read path changes.
--
-- Nullable, and nothing is backfilled: a revision saved before its type was
-- moderated (every existing revision) reads as published when its `status`
-- is 1 and as draft when it is 0, which is what it was. No row is rewritten,
-- and running this twice changes nothing the second time.

ALTER TABLE item_revision ADD COLUMN IF NOT EXISTS moderation_state VARCHAR(32);

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint WHERE conname = 'item_revision_moderation_state_check'
    ) THEN
        ALTER TABLE item_revision ADD CONSTRAINT item_revision_moderation_state_check
            CHECK (moderation_state IS NULL OR moderation_state IN ('draft', 'published', 'archived'));
    END IF;
END $$;
