-- Argus gather query: story list with composite includes (articles nested under stories).
-- This validates the IncludeDefinition feature (Story 20-3 key validation).
-- Forward-only migration; no rollback. Kernel tables are guaranteed to exist.
--
-- Include join note: parent_field="id" (UUID) is matched against
-- child_field="fields.field_story_id" (JSONB text). The kernel's
-- extract_field_value() normalizes all values to strings before comparison,
-- then parses parent UUIDs back via Uuid::parse_str() for the IN filter clause.
-- This string→UUID round-trip is safe for well-formed UUIDs.

INSERT INTO gather_query (query_id, label, description, definition, display, plugin, created, changed)
VALUES (
    'argus_story_list',
    'Stories',
    'Active stories with embedded articles',
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
            },
            {
                "field": "fields.field_active",
                "operator": "equals",
                "value": true,
                "exposed": false,
                "exposed_label": null
            }
        ],
        "sorts": [
            {
                "field": "fields.field_relevance_score",
                "direction": "desc",
                "nulls": null
            }
        ],
        "relationships": [],
        "includes": {
            "articles": {
                "definition": {
                    "base_table": "item",
                    "item_type": "argus_article",
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
                            "field": "fields.field_relevance_score",
                            "direction": "desc",
                            "nulls": null
                        }
                    ],
                    "relationships": [],
                    "includes": {}
                },
                "parent_field": "id",
                "child_field": "fields.field_story_id",
                "singular": false,
                "display": {
                    "format": "list",
                    "items_per_page": 10,
                    "pager": {
                        "enabled": false,
                        "style": "full",
                        "show_count": false
                    },
                    "empty_text": "No articles yet.",
                    "header": null,
                    "footer": null
                }
            }
        }
    }'::jsonb,
    '{
        "format": "list",
        "items_per_page": 20,
        "pager": {
            "enabled": true,
            "style": "full",
            "show_count": true
        },
        "empty_text": "No active stories.",
        "header": null,
        "footer": null
    }'::jsonb,
    'argus',
    EXTRACT(EPOCH FROM NOW())::bigint,
    EXTRACT(EPOCH FROM NOW())::bigint
)
ON CONFLICT (query_id) DO UPDATE SET
    definition = EXCLUDED.definition,
    display = EXCLUDED.display,
    plugin = EXCLUDED.plugin,
    changed = EXCLUDED.changed;

-- argus_feed_list: table format, 20/page, sort by name asc
INSERT INTO gather_query (query_id, label, description, definition, display, plugin, created, changed)
VALUES (
    'argus_feed_list',
    'Feeds',
    'Configured RSS/Atom feed sources',
    '{
        "base_table": "item",
        "item_type": "argus_feed",
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
                "field": "fields.field_name",
                "direction": "asc",
                "nulls": null
            }
        ],
        "relationships": [],
        "includes": {}
    }'::jsonb,
    '{
        "format": "table",
        "items_per_page": 20,
        "pager": {
            "enabled": true,
            "style": "full",
            "show_count": true
        },
        "empty_text": "No feeds configured.",
        "header": null,
        "footer": null
    }'::jsonb,
    'argus',
    EXTRACT(EPOCH FROM NOW())::bigint,
    EXTRACT(EPOCH FROM NOW())::bigint
)
ON CONFLICT (query_id) DO UPDATE SET
    definition = EXCLUDED.definition,
    display = EXCLUDED.display,
    plugin = EXCLUDED.plugin,
    changed = EXCLUDED.changed;
