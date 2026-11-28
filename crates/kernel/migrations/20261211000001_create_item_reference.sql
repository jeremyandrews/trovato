-- B290 — item-to-item reference index for RecordReference fields.
--
-- An item can hold a `RecordReference(target_type)` field whose value is
-- another item's uuid (or an array of them). Nothing recorded which items
-- held such a reference, so nothing could answer "what references this
-- item" — the question the media library's usage count and delete-refusal
-- need for a media item, and a question any future RecordReference target
-- can reuse without its own table.
--
-- Maintained on item create/update (crate::content::media_refs, called from
-- ItemService). Not specific to media: it indexes every RecordReference
-- field value of every content type (CategoryReference is a different field
-- type and is not touched here). Rows are removed automatically when either
-- side is deleted (both FKs cascade).
CREATE TABLE item_reference (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    referencing_item_id UUID NOT NULL REFERENCES item(id) ON DELETE CASCADE,
    referenced_item_id UUID NOT NULL REFERENCES item(id) ON DELETE CASCADE,
    field_name TEXT NOT NULL,
    created BIGINT NOT NULL,
    UNIQUE (referencing_item_id, field_name, referenced_item_id)
);

CREATE INDEX idx_item_reference_referenced ON item_reference(referenced_item_id);
CREATE INDEX idx_item_reference_referencing ON item_reference(referencing_item_id);
