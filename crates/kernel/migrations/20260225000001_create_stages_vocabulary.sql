-- Stage Architecture v2: Convert stages from standalone table to category vocabulary terms.
--
-- Creates the "stages" category and seeds the initial "live" stage term
-- using a deterministic UUIDv7 so all subsequent migrations can reference it.

-- Create "stages" category (weight -100 to sort before user categories)
INSERT INTO category (id, label, description, hierarchy, weight)
VALUES ('stages', 'Stages', 'Content staging workflow stages', 0, -100)
ON CONFLICT (id) DO NOTHING;

-- Seed "live" stage as a category_tag.
-- Use a deterministic UUID so the FK migration can reference it.
-- 0193a5a0-0000-7000-8000-000000000001 is a hand-crafted UUIDv7-shaped value.
INSERT INTO category_tag (id, category_id, label, description, weight, created, changed)
VALUES (
    '0193a5a0-0000-7000-8000-000000000001'::uuid,
    'stages',
    'Live',
    'Published content visible to all visitors',
    0,
    EXTRACT(EPOCH FROM NOW())::bigint,
    EXTRACT(EPOCH FROM NOW())::bigint
)
ON CONFLICT (id) DO NOTHING;
