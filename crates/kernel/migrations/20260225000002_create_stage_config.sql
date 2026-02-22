-- Stage Architecture v2: Stage-specific metadata table.
--
-- Extends category_tag with stage-specific fields that don't belong
-- on the generic tag table: machine_name, visibility, is_default.

CREATE TABLE stage_config (
    -- FK to the category_tag that represents this stage
    tag_id UUID PRIMARY KEY REFERENCES category_tag(id) ON DELETE CASCADE,

    -- Machine-readable identifier (e.g. "live", "draft", "review")
    machine_name VARCHAR(64) NOT NULL UNIQUE,

    -- Stage visibility: 'public' (anonymous), 'internal' (editors), 'accessible' (direct URL only)
    visibility VARCHAR(32) NOT NULL DEFAULT 'internal',

    -- Whether this is the default stage for new content
    is_default BOOLEAN NOT NULL DEFAULT false
);

-- Exactly one stage can be public (the "live" stage)
CREATE UNIQUE INDEX uq_stage_visibility_public
    ON stage_config (visibility) WHERE visibility = 'public';

-- Exactly one stage can be the default
CREATE UNIQUE INDEX uq_stage_is_default
    ON stage_config (is_default) WHERE is_default = true;

-- Seed the "live" stage config
INSERT INTO stage_config (tag_id, machine_name, visibility, is_default)
VALUES (
    '0193a5a0-0000-7000-8000-000000000001'::uuid,
    'live',
    'public',
    true
)
ON CONFLICT (tag_id) DO NOTHING;
