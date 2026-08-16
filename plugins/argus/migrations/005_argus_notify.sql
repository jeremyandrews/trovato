-- Argus milestone 4 (notifications). Forward-only; no rollback.
--
-- Three tables and nothing else that is new in kind: an outbox of decisions, a
-- delivery row per channel per decision, and a rolling health counter per
-- channel. The *configuration* of a channel is not here — it is an Item
-- (argus_notify_channel), for the reason M3 established: the kernel's content
-- forms are the only surface through which an administrator can write anything
-- a plugin owns (M4-DESIGN.md Decision 1, G-NO-PLUGIN-HTTP).
--
-- The same ordering constraint as M3 applies: item_type rows are written at
-- runtime by ContentTypeRegistry::sync_from_plugins, after migrations, so this
-- file cannot create a channel Item. It does not need to — a site with no
-- channels configured simply notifies nobody, which is the correct default.

-- ---------------------------------------------------------------------------
-- The outbox
-- ---------------------------------------------------------------------------
-- One row per decision to notify, written *before* anything is sent. Queue v2
-- delivers at least once (D-47), so the decision has to be idempotent
-- independently of the send: the unique index on (event_type, dedup_key) is
-- what makes a redelivered summarize job record one event rather than two.
--
-- `data` is TEXT holding a JSON object rather than JSONB, matching how M2
-- stores raw model output: the plugin reads and writes it whole through the db
-- host, and a JSONB column would only add a cast at every boundary.
CREATE TABLE IF NOT EXISTS argus_notify_events (
    id           UUID PRIMARY KEY,
    -- 'story.new' | 'story.updated' | 'story.digest' | 'alert.*'
    event_type   TEXT NOT NULL,
    -- 'normal' | 'high'. High bypasses debounce, quiet hours and digesting.
    priority     TEXT NOT NULL DEFAULT 'normal',
    -- The story or feed this is about; NULL for a site-wide alert.
    subject_id   UUID,
    -- The idempotency key, unique with event_type.
    dedup_key    TEXT NOT NULL,
    -- 'pending' | 'sent' | 'suppressed' | 'digested'
    state        TEXT NOT NULL DEFAULT 'pending',
    -- Why it was suppressed, in an operator's terms.
    reason       TEXT,
    title        TEXT NOT NULL DEFAULT '',
    body         TEXT NOT NULL DEFAULT '',
    link         TEXT,
    data         TEXT NOT NULL DEFAULT '{}',
    -- Earliest instant this may be sent; pushed out by quiet hours.
    scheduled_at BIGINT NOT NULL,
    sent_at      BIGINT,
    created      BIGINT NOT NULL,
    changed      BIGINT NOT NULL
);

-- The idempotency guarantee. Without this an at-least-once redelivery of the
-- job that *decided* to notify would notify again.
CREATE UNIQUE INDEX IF NOT EXISTS uniq_argus_notify_events_key
    ON argus_notify_events (event_type, dedup_key);

-- The digest scan: pending, due, digestible, inside the window.
CREATE INDEX IF NOT EXISTS idx_argus_notify_events_pending
    ON argus_notify_events (state, scheduled_at, created);

-- The debounce read: when was this subject last actually told about.
CREATE INDEX IF NOT EXISTS idx_argus_notify_events_subject_sent
    ON argus_notify_events (subject_id, sent_at DESC);

-- ---------------------------------------------------------------------------
-- Deliveries
-- ---------------------------------------------------------------------------
-- One row per (event, channel): what was sent, where, what came back, and how
-- many times it has been tried. `request_url` and `request_body` hold the
-- payload as rendered, written before the attempt — which is what lets an
-- operator (and the end-to-end test) see exactly what a channel was handed even
-- when the send never left the building.
--
-- `id` is a surrogate key because the record admin addresses a row by a single
-- uuid; (event_id, channel_id) is the real identity and carries the unique
-- index the upsert conflicts on.
CREATE TABLE IF NOT EXISTS argus_notify_deliveries (
    id           UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    event_id     UUID NOT NULL,
    channel_id   UUID NOT NULL,
    channel_name TEXT NOT NULL DEFAULT '',
    -- 'delivered' | 'failed' | 'blocked' | 'skipped'
    state        TEXT NOT NULL,
    attempts     INTEGER NOT NULL DEFAULT 0,
    http_status  INTEGER,
    last_error   TEXT,
    request_url  TEXT,
    request_body TEXT,
    created      BIGINT NOT NULL,
    changed      BIGINT NOT NULL
);

CREATE UNIQUE INDEX IF NOT EXISTS uniq_argus_notify_deliveries_pair
    ON argus_notify_deliveries (event_id, channel_id);

-- "What happened to this notification", the operational read.
CREATE INDEX IF NOT EXISTS idx_argus_notify_deliveries_event
    ON argus_notify_deliveries (event_id);

-- "Is this channel healthy", across events.
CREATE INDEX IF NOT EXISTS idx_argus_notify_deliveries_channel_state
    ON argus_notify_deliveries (channel_id, state, changed DESC);

-- ---------------------------------------------------------------------------
-- Channel health
-- ---------------------------------------------------------------------------
-- Bounded on purpose: a counter, one message and two timestamps per channel.
-- A per-channel error *log* is a table that only ever grows, and the delivery
-- rows above already hold the per-event detail.
CREATE TABLE IF NOT EXISTS argus_notify_channels (
    -- The channel Item's id.
    id                   UUID PRIMARY KEY,
    consecutive_failures INTEGER NOT NULL DEFAULT 0,
    last_error           TEXT,
    last_error_at        BIGINT,
    last_success_at      BIGINT,
    changed              BIGINT NOT NULL
);

-- ---------------------------------------------------------------------------
-- Permissions
-- ---------------------------------------------------------------------------
-- The channel type's CRUD permissions come from tap_perm; argus_admin gets them
-- here so the role seeded in 004 can manage channels too. Same caveat as there:
-- the /admin/content screens are gated on users.is_admin rather than on
-- permissions (G-ADMIN-UI-IS-ADMIN-ONLY), so this grants management through the
-- JSON item routes.
INSERT INTO role_permissions (role_id, permission)
SELECT r.id, p.permission
FROM roles r
CROSS JOIN (VALUES
    ('create argus_notify_channel content'),
    ('edit argus_notify_channel content'),
    ('delete argus_notify_channel content'),
    ('view argus_notify_channel content')
) AS p(permission)
WHERE r.name = 'argus_admin'
ON CONFLICT DO NOTHING;

-- ---------------------------------------------------------------------------
-- Admin surface
-- ---------------------------------------------------------------------------
-- The channel list is an Item gather, so unlike the read-only record admin it
-- links to the kernel's own edit form.
INSERT INTO gather_query (query_id, label, description, definition, display, plugin, created, changed)
VALUES (
    'argus_channel_admin',
    'Argus notification channels',
    'Every configured channel, enabled and paused',
    '{
        "base_table": "item",
        "item_type": "argus_notify_channel",
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
        "empty_text": "No notification channels configured yet. Argus is notifying nobody.",
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

-- The notification log, over the record type. Gathers cannot aggregate
-- (G-NO-GATHER-AGGREGATION), so "how many notifications went out today" is the
-- pager count of this list rather than a number in a tile.
INSERT INTO gather_query (query_id, label, description, definition, display, plugin, created, changed)
VALUES (
    'argus_notify_log',
    'Argus notifications',
    'Every notification decision, sent or suppressed',
    '{
        "record_type": "argus_notify_event",
        "fields": [],
        "filters": [
            {
                "field": "state",
                "operator": "contains",
                "value": "",
                "exposed": true,
                "exposed_label": "State"
            }
        ],
        "sorts": [{ "field": "created", "direction": "desc", "nulls": null }],
        "relationships": [],
        "includes": {}
    }'::jsonb,
    '{
        "format": "table",
        "items_per_page": 25,
        "pager": { "enabled": true, "style": "full", "show_count": true },
        "empty_text": "Nothing has been notified yet.",
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

INSERT INTO url_alias (id, source, alias, language, stage_id, created)
VALUES
    (gen_random_uuid(), '/gather/argus_channel_admin', '/admin/argus/channels', 'en',
     '0193a5a0-0000-7000-8000-000000000001', EXTRACT(EPOCH FROM NOW())::bigint),
    (gen_random_uuid(), '/gather/argus_notify_log', '/admin/argus/notifications', 'en',
     '0193a5a0-0000-7000-8000-000000000001', EXTRACT(EPOCH FROM NOW())::bigint)
ON CONFLICT (alias, language, stage_id) DO UPDATE SET source = EXCLUDED.source;
