-- URL aliases for media routes.
-- Forward-only migration; no rollback.

INSERT INTO url_alias (id, source, alias, language, stage_id, created)
VALUES (
    gen_random_uuid(),
    '/gather/media_browser',
    '/media',
    'en',
    '0193a5a0-0000-7000-8000-000000000001',
    EXTRACT(EPOCH FROM NOW())::bigint
)
ON CONFLICT (alias, language, stage_id) DO UPDATE SET
    source = EXCLUDED.source;
