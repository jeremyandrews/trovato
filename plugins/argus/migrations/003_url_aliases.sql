-- Argus URL aliases: /stories and /feeds.
-- Forward-only migration; no rollback. Kernel tables are guaranteed to exist.

-- Note: gen_random_uuid() is evaluated on each run but the id column is NOT in
-- the ON CONFLICT UPDATE SET, so the original UUID is preserved on re-runs.

INSERT INTO url_alias (id, source, alias, language, stage_id, created)
VALUES (
    gen_random_uuid(),
    '/gather/argus_story_list',
    '/stories',
    'en',
    'live',
    EXTRACT(EPOCH FROM NOW())::bigint
)
ON CONFLICT (alias, language, stage_id) DO UPDATE SET
    source = EXCLUDED.source;

INSERT INTO url_alias (id, source, alias, language, stage_id, created)
VALUES (
    gen_random_uuid(),
    '/gather/argus_feed_list',
    '/feeds',
    'en',
    'live',
    EXTRACT(EPOCH FROM NOW())::bigint
)
ON CONFLICT (alias, language, stage_id) DO UPDATE SET
    source = EXCLUDED.source;
