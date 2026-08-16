-- Argus milestone 3 (reader surface, admin, roles). Forward-only; no rollback.
--
-- What this migration can and cannot do is shaped by one ordering fact, argued
-- in M3-DESIGN.md Decision 3: `item.type` references `item_type`, and the
-- `item_type` rows for a plugin's declared types are written at *runtime* by
-- ContentTypeRegistry::sync_from_plugins, which runs after migrations. So this
-- file may create tables, seed gathers/tiles/roles, and relax constraints — but
-- it may not create the feed and topic Items. That is a one-shot inside
-- tap_cron (see config_host::backfill_legacy_config).
--
-- For the same reason the now-inert configuration columns on argus_feeds and
-- argus_topics are NOT dropped here: the backfill that stops using them has not
-- run yet at migration time. Dropping them is a one-line follow-up migration
-- once every install has cycled, and is recorded in M3-FRICTION.md so it is not
-- forgotten.

-- ---------------------------------------------------------------------------
-- argus_feeds becomes the fetch-state table
-- ---------------------------------------------------------------------------
-- From M3 the row is keyed by the feed *Item's* id and carries only state the
-- pipeline owns (M3-DESIGN.md Decision 2). It is now created on demand by the
-- first fetch rather than by an admin inserting a feed, so the columns that
-- used to be supplied at insert time must stop being mandatory.
ALTER TABLE argus_feeds ALTER COLUMN url DROP NOT NULL;
ALTER TABLE argus_feeds ALTER COLUMN topic_id DROP NOT NULL;
ALTER TABLE argus_feeds ALTER COLUMN created SET DEFAULT EXTRACT(EPOCH FROM NOW())::bigint;
ALTER TABLE argus_feeds ALTER COLUMN changed SET DEFAULT EXTRACT(EPOCH FROM NOW())::bigint;

-- The M1 unique index on url was the dedup key for admin-entered feeds. Feed
-- identity now lives on the Item, and every state row created from M3 on has a
-- NULL url, so the index no longer means anything. (Postgres permits repeated
-- NULLs in a unique index, so it would not have blocked anything either — it is
-- dropped because it is misleading, not because it is in the way.)
DROP INDEX IF EXISTS uniq_argus_feeds_url;

-- ---------------------------------------------------------------------------
-- Reader state
-- ---------------------------------------------------------------------------
-- Three plugin-owned tables. Reads reach them through CurrentUser-scoped
-- gathers; writes reach argus_read_state through tap_item_view and reach the
-- other two through nothing at all in 1.0, because no kernel surface lets an
-- authenticated reader write a plugin-owned table (G-NO-PLUGIN-HTTP).

CREATE TABLE IF NOT EXISTS argus_reactions (
    user_id       UUID NOT NULL,
    story_item_id UUID NOT NULL,
    reaction_type TEXT NOT NULL,
    created       BIGINT NOT NULL,
    -- The unique triple the scope asked for: one row per reader per story per
    -- kind, so an at-least-once replay of the same tap cannot double-count a
    -- vote.
    PRIMARY KEY (user_id, story_item_id, reaction_type)
);
-- "Who reacted to this story", for counts on a story page.
CREATE INDEX IF NOT EXISTS idx_argus_reactions_story ON argus_reactions (story_item_id);
-- "What has this reader bookmarked", the gather's access path.
CREATE INDEX IF NOT EXISTS idx_argus_reactions_user_kind
    ON argus_reactions (user_id, reaction_type);

CREATE TABLE IF NOT EXISTS argus_read_state (
    user_id       UUID NOT NULL,
    story_item_id UUID NOT NULL,
    first_seen_at BIGINT NOT NULL,
    last_seen_at  BIGINT NOT NULL,
    view_count    BIGINT NOT NULL DEFAULT 1,
    PRIMARY KEY (user_id, story_item_id)
);
-- "What has this reader seen lately", newest first.
CREATE INDEX IF NOT EXISTS idx_argus_read_state_user_seen
    ON argus_read_state (user_id, last_seen_at DESC);

CREATE TABLE IF NOT EXISTS argus_subscriptions (
    user_id       UUID NOT NULL,
    topic_item_id UUID NOT NULL,
    created       BIGINT NOT NULL,
    PRIMARY KEY (user_id, topic_item_id)
);
CREATE INDEX IF NOT EXISTS idx_argus_subscriptions_topic
    ON argus_subscriptions (topic_item_id);

-- ---------------------------------------------------------------------------
-- Roles
-- ---------------------------------------------------------------------------
-- argus_admin manages feeds and topics; argus_reader reads and discusses.
--
-- Worth knowing before relying on argus_admin: the *admin screens*
-- (/admin/content/...) are gated on the users.is_admin flag, not on permissions
-- (crates/kernel/src/routes/helpers.rs, require_admin). So this role grants feed
-- management through the JSON item routes, which do check permissions, and not
-- through the admin UI. G-ADMIN-UI-IS-ADMIN-ONLY.
INSERT INTO roles (id, name)
VALUES
    ('019a4720-0000-7000-8000-0000000000a1', 'argus_admin'),
    ('019a4720-0000-7000-8000-0000000000a2', 'argus_reader')
ON CONFLICT (name) DO NOTHING;

-- Permissions are matched to the strings tap_perm declares:
-- PermissionDefinition::crud_for_type emits "<verb> <type> content".
INSERT INTO role_permissions (role_id, permission)
SELECT r.id, p.permission
FROM roles r
CROSS JOIN (VALUES
    ('administer argus'),
    ('view argus stories'),
    ('react to argus stories'),
    ('create argus_feed content'),
    ('edit argus_feed content'),
    ('delete argus_feed content'),
    ('view argus_feed content'),
    ('create argus_topic content'),
    ('edit argus_topic content'),
    ('delete argus_topic content'),
    ('view argus_topic content'),
    ('edit argus_story content'),
    ('delete argus_story content'),
    ('view argus_story content')
) AS p(permission)
WHERE r.name = 'argus_admin'
ON CONFLICT DO NOTHING;

INSERT INTO role_permissions (role_id, permission)
SELECT r.id, p.permission
FROM roles r
CROSS JOIN (VALUES
    ('view argus stories'),
    ('react to argus stories')
) AS p(permission)
WHERE r.name = 'argus_reader'
ON CONFLICT DO NOTHING;

-- ---------------------------------------------------------------------------
-- Gathers
-- ---------------------------------------------------------------------------
-- The M1 argus_story_list gather stays as the /stories route and gains what a
-- reader needs: recency order and an exposed topic filter. The two new gathers
-- are the by-topic view and the archive.

-- Story feed: active stories, newest first.
--
-- No exposed topic filter here, and that is a kernel constraint rather than an
-- omission. An exposed `equals` filter whose value is unset stays in the query
-- as `field = ''` — the builder skips empty values for `in`/`full_text_search`
-- but not for `equals` (crates/kernel/src/gather/query_builder.rs) — so a
-- "leave blank for all" filter returns nothing at all. Switching to `contains`
-- would match everything when blank, but the JSONB extraction is NULL for a
-- story with no topic, and `NULL ILIKE '%%'` is not true, so the default view
-- would silently drop every untopiced story. Topic filtering is therefore its
-- own route (/stories/topic?topic=<id>), where the value is always supplied.
-- G-EXPOSED-FILTER-NO-MATCH-ALL.
INSERT INTO gather_query (query_id, label, description, definition, display, plugin, created, changed)
VALUES (
    'argus_story_list',
    'Stories',
    'Active stories, most recent first',
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
                "field": "fields.field_is_active.value",
                "operator": "equals",
                "value": true,
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
    label       = EXCLUDED.label,
    description = EXCLUDED.description,
    definition  = EXCLUDED.definition,
    display     = EXCLUDED.display,
    plugin      = EXCLUDED.plugin,
    changed     = EXCLUDED.changed;

-- Stories by topic. Same shape, but the topic comes from the URL rather than an
-- exposed form control, so /stories/topic?topic=<id> is linkable.
INSERT INTO gather_query (query_id, label, description, definition, display, plugin, created, changed)
VALUES (
    'argus_stories_by_topic',
    'Stories by topic',
    'Active stories for one topic, most recent first',
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
                "field": "fields.field_topic_id.value",
                "operator": "equals",
                "value": { "url_arg": "topic" },
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
        "empty_text": "No stories for this topic.",
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

-- Archive: stories the retention pass has retired (field_is_active false).
INSERT INTO gather_query (query_id, label, description, definition, display, plugin, created, changed)
VALUES (
    'argus_story_archive',
    'Story archive',
    'Stories no longer accepting articles, most recent first',
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
                "field": "fields.field_is_active.value",
                "operator": "equals",
                "value": false,
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
        "empty_text": "Nothing archived yet.",
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

-- Admin lists over the configuration Items. These are ordinary Item gathers, so
-- unlike the read-only record admin they show what an admin is about to edit
-- and link to the kernel's own edit form.
INSERT INTO gather_query (query_id, label, description, definition, display, plugin, created, changed)
VALUES (
    'argus_feed_admin',
    'Argus feeds',
    'Every configured feed, published and paused',
    '{
        "base_table": "item",
        "item_type": "argus_feed",
        "fields": [],
        "filters": [],
        "sorts": [{ "field": "title", "direction": "asc", "nulls": null }],
        "relationships": [],
        "includes": {}
    }'::jsonb,
    '{
        "format": "table",
        "items_per_page": 50,
        "pager": { "enabled": true, "style": "full", "show_count": true },
        "empty_text": "No feeds configured yet.",
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
    'argus_topic_admin',
    'Argus topics',
    'Every configured topic and its relevance criteria',
    '{
        "base_table": "item",
        "item_type": "argus_topic",
        "fields": [],
        "filters": [],
        "sorts": [{ "field": "title", "direction": "asc", "nulls": null }],
        "relationships": [],
        "includes": {}
    }'::jsonb,
    '{
        "format": "table",
        "items_per_page": 50,
        "pager": { "enabled": true, "style": "full", "show_count": true },
        "empty_text": "No topics configured yet.",
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

-- ---------------------------------------------------------------------------
-- Routes
-- ---------------------------------------------------------------------------
INSERT INTO url_alias (id, source, alias, language, stage_id, created)
VALUES
    (gen_random_uuid(), '/gather/argus_stories_by_topic', '/stories/topic', 'en',
     '0193a5a0-0000-7000-8000-000000000001', EXTRACT(EPOCH FROM NOW())::bigint),
    (gen_random_uuid(), '/gather/argus_story_archive', '/stories/archive', 'en',
     '0193a5a0-0000-7000-8000-000000000001', EXTRACT(EPOCH FROM NOW())::bigint),
    (gen_random_uuid(), '/gather/argus_feed_admin', '/admin/argus/feeds', 'en',
     '0193a5a0-0000-7000-8000-000000000001', EXTRACT(EPOCH FROM NOW())::bigint),
    (gen_random_uuid(), '/gather/argus_topic_admin', '/admin/argus/topics', 'en',
     '0193a5a0-0000-7000-8000-000000000001', EXTRACT(EPOCH FROM NOW())::bigint)
ON CONFLICT (alias, language, stage_id) DO UPDATE SET source = EXCLUDED.source;

-- ---------------------------------------------------------------------------
-- Tiles
-- ---------------------------------------------------------------------------
-- The tile subsystem offers four types — custom_html, menu, gather_query and
-- chat — and gathers have no aggregation (no GROUP BY, no COUNT projection), so
-- "articles by pipeline state" cannot be one tile. Each operational tile is
-- therefore a gather_query tile whose pager count is the number; the shape is
-- imposed by the kernel, not chosen. G-NO-GATHER-AGGREGATION.
INSERT INTO gather_query (query_id, label, description, definition, display, plugin, created, changed)
VALUES (
    'argus_stories_today',
    'Stories today',
    'Stories that changed in the last 24 hours',
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
                "field": "changed",
                "operator": "greater_or_equal",
                "value": { "url_arg": "since" },
                "exposed": true,
                "exposed_label": "Changed since"
            }
        ],
        "sorts": [{ "field": "changed", "direction": "desc", "nulls": null }],
        "relationships": [],
        "includes": {}
    }'::jsonb,
    '{
        "format": "list",
        "items_per_page": 10,
        "pager": { "enabled": true, "style": "mini", "show_count": true },
        "empty_text": "No stories yet today.",
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

-- Pipeline health: articles still in flight, over the record type. The pager
-- count is the "how many are queued" figure; the rows show which.
INSERT INTO gather_query (query_id, label, description, definition, display, plugin, created, changed)
VALUES (
    'argus_pipeline_health',
    'Pipeline health',
    'Articles that have not reached a terminal pipeline state',
    '{
        "record_type": "argus_article",
        "fields": [],
        "filters": [
            {
                "field": "pipeline_state",
                "operator": "contains",
                "value": "",
                "exposed": true,
                "exposed_label": "State"
            }
        ],
        "sorts": [{ "field": "published_at", "direction": "desc", "nulls": null }],
        "relationships": [],
        "includes": {}
    }'::jsonb,
    '{
        "format": "table",
        "items_per_page": 10,
        "pager": { "enabled": true, "style": "mini", "show_count": true },
        "empty_text": "Nothing in flight.",
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

INSERT INTO tile (id, machine_name, label, region, tile_type, config, weight, status, created, changed)
VALUES
    (
        '019a4720-0000-7000-8000-0000000000b1',
        'argus_stories_today',
        'Stories today',
        'sidebar_first',
        'gather_query',
        '{"query_id": "argus_stories_today"}'::jsonb,
        0,
        1,
        EXTRACT(EPOCH FROM NOW())::bigint,
        EXTRACT(EPOCH FROM NOW())::bigint
    ),
    (
        '019a4720-0000-7000-8000-0000000000b2',
        'argus_pipeline_health',
        'Argus pipeline health',
        'sidebar_first',
        'gather_query',
        '{"query_id": "argus_pipeline_health"}'::jsonb,
        1,
        1,
        EXTRACT(EPOCH FROM NOW())::bigint,
        EXTRACT(EPOCH FROM NOW())::bigint
    ),
    (
        '019a4720-0000-7000-8000-0000000000b3',
        'argus_top_topics',
        'Argus topics',
        'sidebar_first',
        'gather_query',
        '{"query_id": "argus_topic_admin"}'::jsonb,
        2,
        1,
        EXTRACT(EPOCH FROM NOW())::bigint,
        EXTRACT(EPOCH FROM NOW())::bigint
    )
-- Keyed on the primary key rather than machine_name: the unique index the
-- tile migration declares on (machine_name, stage_id) is not present on a
-- migrated database, so it is not a usable conflict target.
ON CONFLICT (id) DO UPDATE SET
    machine_name = EXCLUDED.machine_name,
    label     = EXCLUDED.label,
    region    = EXCLUDED.region,
    tile_type = EXCLUDED.tile_type,
    config    = EXCLUDED.config,
    weight    = EXCLUDED.weight,
    changed   = EXCLUDED.changed;
