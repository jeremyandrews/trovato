-- Argus gather queries and URL aliases (M1-11). Forward-only; no rollback.
--
-- Two gathers:
--   argus_article_list — over the `argus_article` RECORD type (not the item
--     table): `record_type` in the definition makes GatherService resolve the
--     name against the RecordTypeRegistry, query the record's base table, and
--     rewrite the logical filter/sort names through the record field map. Two
--     exposed filters (topic, state) default to match-all, so the unfiltered
--     list returns every article; supplying a value filters.
--   argus_story_list — a plain Item gather over the `argus_story` content type.

INSERT INTO gather_query (query_id, label, description, definition, display, plugin, created, changed)
VALUES (
    'argus_article_list',
    'Articles',
    'Scored articles, most relevant first; filter by topic and pipeline state',
    '{
        "record_type": "argus_article",
        "fields": [],
        "filters": [
            {
                "field": "topic_id",
                "operator": "equals",
                "value": "",
                "exposed": true,
                "exposed_label": "Topic"
            },
            {
                "field": "pipeline_state",
                "operator": "contains",
                "value": "",
                "exposed": true,
                "exposed_label": "State"
            }
        ],
        "sorts": [
            {
                "field": "relevance_score",
                "direction": "desc",
                "nulls": null
            }
        ],
        "relationships": [],
        "includes": {}
    }'::jsonb,
    '{
        "format": "table",
        "items_per_page": 50,
        "pager": {
            "enabled": true,
            "style": "full",
            "show_count": true
        },
        "empty_text": "No articles yet.",
        "header": null,
        "footer": null
    }'::jsonb,
    'argus',
    EXTRACT(EPOCH FROM NOW())::bigint,
    EXTRACT(EPOCH FROM NOW())::bigint
)
ON CONFLICT (query_id) DO UPDATE SET
    definition = EXCLUDED.definition,
    display    = EXCLUDED.display,
    plugin     = EXCLUDED.plugin,
    changed    = EXCLUDED.changed;

INSERT INTO gather_query (query_id, label, description, definition, display, plugin, created, changed)
VALUES (
    'argus_story_list',
    'Stories',
    'Published stories',
    '{
        "base_table": "item",
        "item_type": "argus_story",
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
                "field": "changed",
                "direction": "desc",
                "nulls": null
            }
        ],
        "relationships": [],
        "includes": {}
    }'::jsonb,
    '{
        "format": "list",
        "items_per_page": 20,
        "pager": {
            "enabled": true,
            "style": "full",
            "show_count": true
        },
        "empty_text": "No stories yet.",
        "header": null,
        "footer": null
    }'::jsonb,
    'argus',
    EXTRACT(EPOCH FROM NOW())::bigint,
    EXTRACT(EPOCH FROM NOW())::bigint
)
ON CONFLICT (query_id) DO UPDATE SET
    definition = EXCLUDED.definition,
    display    = EXCLUDED.display,
    plugin     = EXCLUDED.plugin,
    changed    = EXCLUDED.changed;

-- Human-friendly routes → gather queries. id is preserved on re-run (not in the
-- ON CONFLICT UPDATE SET), Live stage.
INSERT INTO url_alias (id, source, alias, language, stage_id, created)
VALUES (
    gen_random_uuid(),
    '/gather/argus_article_list',
    '/articles',
    'en',
    '0193a5a0-0000-7000-8000-000000000001',
    EXTRACT(EPOCH FROM NOW())::bigint
)
ON CONFLICT (alias, language, stage_id) DO UPDATE SET source = EXCLUDED.source;

INSERT INTO url_alias (id, source, alias, language, stage_id, created)
VALUES (
    gen_random_uuid(),
    '/gather/argus_story_list',
    '/stories',
    'en',
    '0193a5a0-0000-7000-8000-000000000001',
    EXTRACT(EPOCH FROM NOW())::bigint
)
ON CONFLICT (alias, language, stage_id) DO UPDATE SET source = EXCLUDED.source;
