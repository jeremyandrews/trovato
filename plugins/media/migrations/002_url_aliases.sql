-- URL aliases for media routes.
-- Forward-only migration; no rollback.

INSERT INTO url_alias (id, source, alias, language, stage_id, created)
VALUES (
    gen_random_uuid(),
    '/gather/media_browser',
    '/media',
    'en',
    'live',
    EXTRACT(EPOCH FROM NOW())::bigint
)
ON CONFLICT (alias, language, stage_id) DO UPDATE SET
    source = EXCLUDED.source;
