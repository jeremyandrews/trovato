-- How many images without a text alternative an item's rich text fields hold
-- (B251), kept so the accessibility report reads a stored count and never
-- parses an item's HTML on page load. Written by the item service on every
-- create and update (the same place that drives search indexing and
-- embedding), one row per item, upserted; removed with its item.
CREATE TABLE IF NOT EXISTS item_accessibility (
    item_id UUID PRIMARY KEY REFERENCES item(id) ON DELETE CASCADE,
    images_missing_alt INTEGER NOT NULL DEFAULT 0
);

-- Only an item with at least one finding is ever selected for the report.
CREATE INDEX IF NOT EXISTS idx_item_accessibility_missing
    ON item_accessibility (images_missing_alt)
    WHERE images_missing_alt > 0;
