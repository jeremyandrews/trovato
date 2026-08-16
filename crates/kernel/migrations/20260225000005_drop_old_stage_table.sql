-- Stage Architecture v2: Drop the old standalone stage table.
--
-- All FK references have been migrated to category_tag UUIDs.
-- The stage table is no longer needed.

DROP TABLE IF EXISTS stage;
