-- Admin listing for redirects.
-- Forward-only migration; no rollback.

INSERT INTO gather_query (query_id, label, description, definition, display, plugin, created, changed)
VALUES (
    'redirect_admin_list',
    'Redirects',
    'All configured redirects',
    '{
        "base_table": "redirect",
        "item_type": null,
        "fields": [],
        "filters": [],
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
        "empty_text": "No redirects configured.",
        "header": null,
        "footer": null
    }'::jsonb,
    'redirects',
    EXTRACT(EPOCH FROM NOW())::bigint,
    EXTRACT(EPOCH FROM NOW())::bigint
)
ON CONFLICT (query_id) DO UPDATE SET
    definition = EXCLUDED.definition,
    display = EXCLUDED.display,
    plugin = EXCLUDED.plugin,
    changed = EXCLUDED.changed;
