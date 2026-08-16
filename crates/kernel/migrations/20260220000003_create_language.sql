-- Language table for multilingual infrastructure
-- Story 22.5: Language Column Infrastructure

CREATE TABLE IF NOT EXISTS language (
    id VARCHAR(12) PRIMARY KEY,
    label VARCHAR(255) NOT NULL,
    weight INT NOT NULL DEFAULT 0,
    is_default BOOLEAN NOT NULL DEFAULT false,
    direction VARCHAR(3) NOT NULL DEFAULT 'ltr',
    CONSTRAINT chk_language_direction CHECK (direction IN ('ltr', 'rtl'))
);

-- Ensure at most one default language
CREATE UNIQUE INDEX IF NOT EXISTS idx_language_single_default
    ON language (is_default) WHERE is_default = true;

-- Seed English as the default language
INSERT INTO language (id, label, weight, is_default, direction)
VALUES ('en', 'English', 0, true, 'ltr')
ON CONFLICT (id) DO NOTHING;

-- Add language column to item table (idempotent)
DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM information_schema.columns
        WHERE table_schema = current_schema()
        AND table_name = 'item' AND column_name = 'language'
    ) THEN
        ALTER TABLE item ADD COLUMN language VARCHAR(12) NOT NULL DEFAULT 'en'
            REFERENCES language(id) ON DELETE RESTRICT;
    END IF;
END $$;

CREATE INDEX IF NOT EXISTS idx_item_language ON item(language);

-- Clean up url_alias language values and add FK in a single locked operation.
-- LOCK TABLE prevents concurrent inserts with invalid language values between
-- the cleanup UPDATE and FK creation.
DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM information_schema.table_constraints
        WHERE constraint_schema = current_schema()
        AND constraint_name = 'fk_url_alias_language'
        AND table_name = 'url_alias'
    ) THEN
        LOCK TABLE url_alias IN EXCLUSIVE MODE;

        UPDATE url_alias SET language = 'en'
            WHERE language IS NULL OR language = '' OR language NOT IN (SELECT id FROM language);

        ALTER TABLE url_alias ADD CONSTRAINT fk_url_alias_language
            FOREIGN KEY (language) REFERENCES language(id) ON DELETE RESTRICT;
    END IF;
END $$;
