-- The item view counter (B160): how many times each item has been viewed,
-- Drupal's statistics `node_counter`. Written only while the site counts views
-- ("Count content views" at /admin/config/site, off by default), one upsert per
-- item view, and removed with its item.
CREATE TABLE IF NOT EXISTS item_counter (
    item_id UUID PRIMARY KEY REFERENCES item(id) ON DELETE CASCADE,
    total_count BIGINT NOT NULL DEFAULT 0,
    -- Unix seconds of the latest counted view.
    last_viewed BIGINT NOT NULL DEFAULT 0
);
