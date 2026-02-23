-- Seed "Upcoming Conferences" gather query and /conferences URL alias (Story 29.3)
-- Follows the blog plugin pattern (plugins/blog/migrations/001_seed_gather_query.sql).
-- Runs after 20260225000003 (stage FK migration) so url_alias.stage_id is UUID.
-- Forward-only migration; no rollback. Kernel tables are guaranteed to exist.

INSERT INTO gather_query (query_id, label, description, definition, display, plugin, created, changed)
VALUES (
    'upcoming_conferences',
    'Upcoming Conferences',
    'Published conferences sorted by start date',
    '{
        "base_table": "item",
        "item_type": "conference",
        "fields": [],
        "filters": [
            {
                "field": "status",
                "operator": "equals",
                "value": 1,
                "exposed": false,
                "exposed_label": null
            }
        ],
        "sorts": [
            {
                "field": "fields.field_start_date",
                "direction": "asc",
                "nulls": null
            }
        ],
        "relationships": [],
        "includes": {},
        "stage_aware": true
    }'::jsonb,
    '{
        "format": "table",
        "items_per_page": 25,
        "pager": {
            "enabled": true,
            "style": "full",
            "show_count": true
        },
        "empty_text": "No conferences found.",
        "header": null,
        "footer": null
    }'::jsonb,
    'core',
    EXTRACT(EPOCH FROM NOW())::bigint,
    EXTRACT(EPOCH FROM NOW())::bigint
)
ON CONFLICT (query_id) DO UPDATE SET
    definition = EXCLUDED.definition,
    display = EXCLUDED.display,
    plugin = EXCLUDED.plugin,
    changed = EXCLUDED.changed;

-- URL alias: /conferences → /gather/upcoming_conferences
-- The path alias middleware rewrites /conferences to /gather/upcoming_conferences
-- transparently, so visitors see the clean URL.
-- Uses idempotent guard (the unique constraint on (alias, language, stage_id)
-- was lost during the stage FK migration in 20260225000003).
INSERT INTO url_alias (id, source, alias, language, stage_id, created)
SELECT
    gen_random_uuid(),
    '/gather/upcoming_conferences',
    '/conferences',
    'en',
    '0193a5a0-0000-7000-8000-000000000001'::uuid,
    EXTRACT(EPOCH FROM NOW())::bigint
WHERE NOT EXISTS (
    SELECT 1 FROM url_alias
    WHERE alias = '/conferences'
      AND language = 'en'
      AND stage_id = '0193a5a0-0000-7000-8000-000000000001'::uuid
);
