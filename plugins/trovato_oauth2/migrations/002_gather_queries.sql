-- Admin listing for OAuth clients.
-- Forward-only migration; no rollback.

INSERT INTO gather_query (query_id, label, description, definition, display, plugin, created, changed)
VALUES (
    'oauth_client_admin_list',
    'OAuth Clients',
    'All registered OAuth clients',
    '{
        "base_table": "oauth_client",
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
        "empty_text": "No OAuth clients registered.",
        "header": null,
        "footer": null
    }'::jsonb,
    'oauth2',
    EXTRACT(EPOCH FROM NOW())::bigint,
    EXTRACT(EPOCH FROM NOW())::bigint
)
ON CONFLICT (query_id) DO UPDATE SET
    definition = EXCLUDED.definition,
    display = EXCLUDED.display,
    plugin = EXCLUDED.plugin,
    changed = EXCLUDED.changed;
