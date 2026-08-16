-- Category table
-- Categories are named collections of tags (e.g., "Topics", "Regions")

CREATE TABLE category (
    id VARCHAR(32) PRIMARY KEY,
    label VARCHAR(255) NOT NULL,
    description TEXT,
    hierarchy SMALLINT NOT NULL DEFAULT 0,  -- 0=flat, 1=single parent, 2=multiple parents (DAG)
    weight SMALLINT NOT NULL DEFAULT 0
);

CREATE INDEX idx_category_weight ON category(weight);

-- Seed some default categories
INSERT INTO category (id, label, description, hierarchy)
VALUES
    ('tags', 'Tags', 'Free-form tags for content classification', 0),
    ('topics', 'Topics', 'Hierarchical content topics', 2);
