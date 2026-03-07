-- Add optional slug column to category_tag for URL-friendly lookups.
-- Slugs enable generic gather route aliases (e.g. /topics/{slug}) without
-- plugin-specific code in the kernel.

ALTER TABLE category_tag ADD COLUMN slug VARCHAR(128);

-- Unique within a category so slug-based lookups are unambiguous.
CREATE UNIQUE INDEX idx_category_tag_category_slug
    ON category_tag(category_id, slug)
    WHERE slug IS NOT NULL;
