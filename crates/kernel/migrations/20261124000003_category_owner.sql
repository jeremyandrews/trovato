-- B108: owned vocabularies.
--
-- A vocabulary (a `category` row) may belong to an owner a plugin declares:
-- a group, a club, a project. `owner_type` is the kind of owner, as the
-- plugin's manifest declares it; `owner_id` is the plugin's id for the one
-- owner. Both are NULL for a site-wide vocabulary, which is what every
-- existing row is, so an existing site behaves exactly as before.

ALTER TABLE category
    ADD COLUMN owner_type VARCHAR(64),
    ADD COLUMN owner_id VARCHAR(255);

ALTER TABLE category
    ADD CONSTRAINT category_owner_pair
    CHECK ((owner_type IS NULL) = (owner_id IS NULL));

CREATE INDEX idx_category_owner ON category (owner_type, owner_id)
    WHERE owner_type IS NOT NULL;
