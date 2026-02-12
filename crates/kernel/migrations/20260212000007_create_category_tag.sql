-- Category Tag table
-- Tags are individual entries within a category

CREATE TABLE category_tag (
    id UUID PRIMARY KEY,
    category_id VARCHAR(32) NOT NULL REFERENCES category(id) ON DELETE CASCADE,
    label VARCHAR(255) NOT NULL,
    description TEXT,
    weight SMALLINT NOT NULL DEFAULT 0,
    created BIGINT NOT NULL,
    changed BIGINT NOT NULL
);

CREATE INDEX idx_category_tag_category ON category_tag(category_id);
CREATE INDEX idx_category_tag_weight ON category_tag(weight);
CREATE INDEX idx_category_tag_label ON category_tag(label);
CREATE INDEX idx_category_tag_category_weight ON category_tag(category_id, weight);
