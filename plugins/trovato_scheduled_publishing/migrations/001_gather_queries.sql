-- Scheduled items listing.
-- Forward-only migration; no rollback.

INSERT INTO gather_query (query_id, label, description, definition, display, plugin, created, changed)
VALUES (
    'scheduled_items',
    'Scheduled Items',
    'Items scheduled for future publish or unpublish',
    '{
        "base_table": "item",
        "item_type": null,
        "fields": [],
        "filters": [
            {
                "field": "fields->>''field_publish_on''",
                "operator": "is_not_null",
                "value": null,
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
        "format": "table",
        "items_per_page": 50,
        "pager": {
            "enabled": true,
            "style": "full",
            "show_count": true
        },
        "empty_text": "No scheduled items.",
        "header": null,
        "footer": null
    }'::jsonb,
    'scheduled_publishing',
    EXTRACT(EPOCH FROM NOW())::bigint,
    EXTRACT(EPOCH FROM NOW())::bigint
)
ON CONFLICT (query_id) DO UPDATE SET
    definition = EXCLUDED.definition,
    display = EXCLUDED.display,
    plugin = EXCLUDED.plugin,
    changed = EXCLUDED.changed;
