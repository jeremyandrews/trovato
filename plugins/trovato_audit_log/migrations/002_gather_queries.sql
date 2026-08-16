-- Admin listing for audit log.
-- Forward-only migration; no rollback.

INSERT INTO gather_query (query_id, label, description, definition, display, plugin, created, changed)
VALUES (
    'audit_log_admin',
    'Audit Log',
    'Recent audit log entries',
    '{
        "base_table": "audit_log",
        "item_type": null,
        "fields": [],
        "filters": [
            {
                "field": "entity_type",
                "operator": "equals",
                "value": null,
                "exposed": true,
                "exposed_label": "Entity type"
            },
            {
                "field": "action",
                "operator": "equals",
                "value": null,
                "exposed": true,
                "exposed_label": "Action"
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
        "empty_text": "No audit log entries.",
        "header": null,
        "footer": null
    }'::jsonb,
    'audit_log',
    EXTRACT(EPOCH FROM NOW())::bigint,
    EXTRACT(EPOCH FROM NOW())::bigint
)
ON CONFLICT (query_id) DO UPDATE SET
    definition = EXCLUDED.definition,
    display = EXCLUDED.display,
    plugin = EXCLUDED.plugin,
    changed = EXCLUDED.changed;
