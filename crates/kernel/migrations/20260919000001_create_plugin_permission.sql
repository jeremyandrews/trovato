-- Declarations of the permissions plugins own, collected from `tap_perm`.
-- Forward-only migration; no rollback.
--
-- This table is a cache of *declarations*, never a grant. `role_permissions`
-- remains the only place a permission is actually held by anyone, and nothing
-- here grants, revokes or implies a grant.
--
-- It exists because a declaration has two consumers that do not share a
-- process. The running server needs it to render the permission grid; `config
-- import` needs it to validate a `role.*.yml` file's `permissions` list, and
-- that runs as a CLI with a pool and no AppState. An in-memory registry serves
-- the first and is invisible to the second, which would leave a plugin's
-- permission unnameable in a config file.

CREATE TABLE IF NOT EXISTS plugin_permission (
    name VARCHAR(255) PRIMARY KEY,
    plugin VARCHAR(255) NOT NULL,
    description TEXT NOT NULL DEFAULT ''
);

-- A refresh replaces one plugin's rows at a time, so the plugin column is the
-- access path, not the primary key.
CREATE INDEX IF NOT EXISTS idx_plugin_permission_plugin
    ON plugin_permission (plugin);
