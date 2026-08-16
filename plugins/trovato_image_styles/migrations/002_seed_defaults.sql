-- Seed default image styles.
-- Forward-only migration; no rollback.

INSERT INTO image_style (id, name, label, effects, created, changed) VALUES
(gen_random_uuid(), 'thumbnail', 'Thumbnail (100x100)', '[{"type": "crop", "width": 100, "height": 100}]'::jsonb, EXTRACT(EPOCH FROM NOW())::bigint, EXTRACT(EPOCH FROM NOW())::bigint),
(gen_random_uuid(), 'medium', 'Medium (500w)', '[{"type": "scale", "width": 500}]'::jsonb, EXTRACT(EPOCH FROM NOW())::bigint, EXTRACT(EPOCH FROM NOW())::bigint),
(gen_random_uuid(), 'large', 'Large (1000w)', '[{"type": "scale", "width": 1000}]'::jsonb, EXTRACT(EPOCH FROM NOW())::bigint, EXTRACT(EPOCH FROM NOW())::bigint)
ON CONFLICT (name) DO NOTHING;
