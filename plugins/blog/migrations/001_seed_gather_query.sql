-- Seed blog_listing gather query and /blog URL alias.
-- Moves inline kernel registration into proper plugin migration.
-- Forward-only migration; no rollback. Kernel tables are guaranteed to exist.

INSERT INTO gather_query (query_id, label, description, definition, display, plugin, created, changed)
VALUES (
    'blog_listing',
    'Blog',
    'Recent blog posts',
    '{
        "base_table": "item",
        "item_type": "blog",
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
                "field": "created",
                "direction": "desc",
                "nulls": null
            }
        ],
        "relationships": [],
        "includes": {}
    }'::jsonb,
    '{
        "format": "list",
        "items_per_page": 10,
        "pager": {
            "enabled": true,
            "style": "full",
            "show_count": true
        },
        "empty_text": "No blog posts yet.",
        "header": null,
        "footer": null
    }'::jsonb,
    'blog',
    EXTRACT(EPOCH FROM NOW())::bigint,
    EXTRACT(EPOCH FROM NOW())::bigint
)
ON CONFLICT (query_id) DO UPDATE SET
    definition = EXCLUDED.definition,
    display = EXCLUDED.display,
    plugin = EXCLUDED.plugin,
    changed = EXCLUDED.changed;

-- URL alias: /blog → /gather/blog_listing
-- Note: gen_random_uuid() is evaluated on each run but the id column is NOT in
-- the ON CONFLICT UPDATE SET, so the original UUID is preserved on re-runs.
INSERT INTO url_alias (id, source, alias, language, stage_id, created)
VALUES (
    gen_random_uuid(),
    '/gather/blog_listing',
    '/blog',
    'en',
    'live',
    EXTRACT(EPOCH FROM NOW())::bigint
)
ON CONFLICT (alias, language, stage_id) DO UPDATE SET
    source = EXCLUDED.source;
