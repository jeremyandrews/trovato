-- trovato:contract 0.105.0: 0.105.0 writes only draft, published and archived and never relies on this check refusing other values

-- Configurable moderation workflows (B301): the states and transitions a
-- content type's items move through become site configuration, one variable,
-- `moderation.workflows`, instead of the fixed Draft/Published/Archived
-- compiled into the kernel. See `docs/design/moderation-workflows.md`.
--
-- The check this drops only ever refused a value the kernel itself never
-- wrote, so dropping it changes nothing for a site that keeps the three
-- built-in states; it only stops refusing a state a site's own workflow adds.

ALTER TABLE item_revision DROP CONSTRAINT IF EXISTS item_revision_moderation_state_check;

-- Seeded with exactly `content::moderation::builtin_editorial()`, so an
-- upgraded site reads the same workflow it always had, under its own name
-- (`editorial`) and with the same permission names
-- (`use editorial transition <id>`). `ON CONFLICT DO NOTHING`: a site that
-- already stored this key (a rerun, or one that imported its own value first)
-- keeps what it has.
INSERT INTO site_config (key, value) VALUES (
    'moderation.workflows',
    '{
        "workflows": [
            {
                "id": "editorial",
                "label": "Editorial",
                "default_state": "draft",
                "states": [
                    {"id": "draft", "label": "Draft", "published": false, "default_revision": false},
                    {"id": "published", "label": "Published", "published": true, "default_revision": true},
                    {"id": "archived", "label": "Archived", "published": false, "default_revision": true}
                ],
                "transitions": [
                    {"id": "create_new_draft", "label": "Create New Draft", "from": ["draft", "published"], "to": "draft"},
                    {"id": "publish", "label": "Publish", "from": ["draft", "published"], "to": "published"},
                    {"id": "archive", "label": "Archive", "from": ["published"], "to": "archived"},
                    {"id": "archived_draft", "label": "Restore to Draft", "from": ["archived"], "to": "draft"},
                    {"id": "archived_published", "label": "Restore", "from": ["archived"], "to": "published"}
                ]
            }
        ]
    }'::jsonb
) ON CONFLICT (key) DO NOTHING;
