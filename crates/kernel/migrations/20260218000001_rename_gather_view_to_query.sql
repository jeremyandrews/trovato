-- Rename gather_view table to gather_query
-- Renames view_id column to query_id to match updated Gather terminology

ALTER TABLE gather_view RENAME TO gather_query;
ALTER TABLE gather_query RENAME COLUMN view_id TO query_id;
ALTER INDEX idx_view_plugin RENAME TO idx_query_plugin;
