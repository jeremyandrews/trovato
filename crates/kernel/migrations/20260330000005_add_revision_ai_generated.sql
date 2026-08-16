-- Add AI-generated flag to revisions (Story 45.2).
-- Set to TRUE when the revision was created via an ai_request() call chain.
-- FALSE for manual saves. Conservative: any AI involvement flags it.

ALTER TABLE item_revision ADD COLUMN ai_generated BOOLEAN DEFAULT FALSE;

-- Immutability trigger (Story 45.3): prevent UPDATE on existing revisions.
-- DELETE still allowed (CASCADE from item). INSERT still allowed.
-- To fix data in migrations, temporarily disable:
--   ALTER TABLE item_revision DISABLE TRIGGER item_revision_immutable;
--   ... fix data ...
--   ALTER TABLE item_revision ENABLE TRIGGER item_revision_immutable;

CREATE OR REPLACE FUNCTION prevent_item_revision_update()
RETURNS TRIGGER AS $$
BEGIN
    RAISE EXCEPTION 'item_revision rows are immutable — updates are not permitted';
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER item_revision_immutable
    BEFORE UPDATE ON item_revision
    FOR EACH ROW
    EXECUTE FUNCTION prevent_item_revision_update();
