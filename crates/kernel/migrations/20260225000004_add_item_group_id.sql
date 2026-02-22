-- Stage Architecture v2: Add item_group_id for multi-stage item copies.
--
-- item_group_id links copies of the same logical item across stages
-- (like git branches). For existing items, group_id = own id.

ALTER TABLE item ADD COLUMN item_group_id UUID;

-- Existing items are each their own group
UPDATE item SET item_group_id = id;

ALTER TABLE item ALTER COLUMN item_group_id SET NOT NULL;
-- Default gen_random_uuid() provides a safety-net for direct SQL inserts.
-- The application always sets item_group_id = item.id (UUIDv7) on create;
-- this v4 default is only hit by raw SQL and is acceptable since group IDs
-- are identifiers, not sorted by creation time.
ALTER TABLE item ALTER COLUMN item_group_id SET DEFAULT gen_random_uuid();

CREATE INDEX idx_item_group ON item(item_group_id);
