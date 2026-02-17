-- Seed gather queries for category and term admin listings.
-- Forward-only migration; no rollback.

INSERT INTO gather_query (query_id, label, description, definition, display, plugin, created, changed)
VALUES (
    'category_admin_list',
    'Categories',
    'All category vocabularies',
    '{
        "base_table": "category",
        "item_type": null,
        "fields": [],
        "filters": [],
        "sorts": [
            {
                "field": "weight",
                "direction": "asc",
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
            "enabled": false,
            "style": "full",
            "show_count": false
        },
        "empty_text": "No categories defined.",
        "header": null,
        "footer": null
    }'::jsonb,
    'categories',
    EXTRACT(EPOCH FROM NOW())::bigint,
    EXTRACT(EPOCH FROM NOW())::bigint
)
ON CONFLICT (query_id) DO UPDATE SET
    definition = EXCLUDED.definition,
    display = EXCLUDED.display,
    plugin = EXCLUDED.plugin,
    changed = EXCLUDED.changed;
