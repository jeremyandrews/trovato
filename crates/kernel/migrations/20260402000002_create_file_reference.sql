-- FR-8 Story 3.5 — file→item reverse reference index.
--
-- The kernel had no file→item link: file_managed.owner_id is the *uploader*,
-- not an owning item, and items reference files only as local:// URIs / /files/
-- public URLs embedded in their field values. The serve path (GET /files/{path})
-- needs to know which items reference a file to enforce the any-referencing
-- policy (D-29): a file is servable iff ANY referencing item is accessible to
-- the viewer; a file referenced by no item is servable only to its uploader
-- (owner_id) and admins.
--
-- Maintained on item create/update/delete (kernel item write paths). Rows are
-- removed automatically when either side is deleted (both FKs cascade).

CREATE TABLE file_reference (
    file_id UUID NOT NULL REFERENCES file_managed(id) ON DELETE CASCADE,
    item_id UUID NOT NULL REFERENCES item(id) ON DELETE CASCADE,
    PRIMARY KEY (file_id, item_id)
);

CREATE INDEX idx_file_reference_file ON file_reference(file_id);
CREATE INDEX idx_file_reference_item ON file_reference(item_id);

-- Backfill current references from stored item field values. An item references
-- a file when the file's storage path (its uri minus the 'local://' scheme, 8
-- chars) appears anywhere in the item's serialized fields — this matches both
-- local:// URIs (File fields) and /files/ public URLs (block-editor/rich
-- content) in one pass, and the uuid embedded in every path makes a spurious
-- substring match effectively impossible. strpos is a literal substring match
-- (no LIKE wildcard hazards from '_' in filenames).
INSERT INTO file_reference (file_id, item_id)
SELECT DISTINCT f.id, i.id
FROM file_managed f
JOIN item i ON strpos(i.fields::text, substring(f.uri FROM 9)) > 0
ON CONFLICT DO NOTHING;
