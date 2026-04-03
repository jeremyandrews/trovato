-- Seed gather query for comment admin listing.
-- Forward-only migration; no rollback.

INSERT INTO gather_query (query_id, label, description, definition, display, plugin, created, changed)
VALUES (
    'comment_admin_list',
    'Comments',
    'All comments for moderation',
    '{
        "base_table": "comment",
        "item_type": null,
        "fields": [],
        "filters": [
            {
                "field": "status",
                "operator": "equals",
                "value": null,
                "exposed": true,
                "exposed_label": "Status"
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
        "empty_text": "No comments.",
        "header": null,
        "footer": null
    }'::jsonb,
    'comments',
    EXTRACT(EPOCH FROM NOW())::bigint,
    EXTRACT(EPOCH FROM NOW())::bigint
)
ON CONFLICT (query_id) DO UPDATE SET
    definition = EXCLUDED.definition,
    display = EXCLUDED.display,
    plugin = EXCLUDED.plugin,
    changed = EXCLUDED.changed;
