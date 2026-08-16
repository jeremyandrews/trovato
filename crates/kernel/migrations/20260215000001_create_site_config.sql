-- Site configuration table for installation status and site settings
-- Epic 17: Installer & Setup Experience

CREATE TABLE site_config (
    -- Configuration key (e.g., 'site_name', 'installed')
    key VARCHAR(64) PRIMARY KEY,

    -- Configuration value (JSON to support various types)
    value JSONB NOT NULL,

    -- When this config was last updated
    updated TIMESTAMP WITH TIME ZONE NOT NULL DEFAULT NOW()
);

-- Index for quick lookups
CREATE INDEX idx_site_config_updated ON site_config(updated);

-- Insert default installation status (not installed)
-- This will be updated to true after the installer completes
INSERT INTO site_config (key, value) VALUES
    ('installed', 'false'::jsonb),
    ('site_name', '"Trovato"'::jsonb),
    ('site_slogan', '""'::jsonb),
    ('site_mail', '""'::jsonb);
