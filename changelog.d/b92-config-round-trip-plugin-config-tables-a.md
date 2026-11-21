Config export and import now carry every kind of configuration, the kernel's or
a plugin's, and no runtime state.

Plugin configuration did not round trip. Image styles, redirects, webhooks,
OAuth clients, configuration translations and interface string translations
live in tables a plugin's migrations create, and the list of config entity
types (`ENTITY_TYPE_ORDER`) was a kernel constant with no way for a plugin to
add to it; which plugins were enabled did not travel either. A manifest can now
name its configuration tables under `[config] tables` (additive; plugin API
0.106), and export writes each row as `<table>.<primary key>.yml`, a map of
column to value converted by Postgres itself (`to_jsonb` out,
`jsonb_populate_record` in), after the kernel's types. A `plugin_status` entity
records whether each plugin is enabled. Import applies it first, runs the
enabled plugins' migrations as boot would, and checks every row before writing
anything: its plugin must end up enabled, every column must exist, and each
value must fit its column. The six in-tree plugins that own configuration
declare their tables.

Variable export dumped all of `site_config`, so `installed`, `update_status`,
`update_check` and `queue_runner`, which describe one deployment rather than the
site, went into every config set and came back out on the next install.
`RUNTIME_STATE_KEYS` names them; export skips them and import ignores them with
a warning.

An export would not import into another install. Plugin migrations seed roles,
URL aliases and image styles under random ids, so the same role, alias or style
had a different id on the target and the import hit the unique name, or kept the
target's id. A role, URL alias or plugin row now takes over the row holding its
name, under the id its file names, with the role's grants and members moved
across; role, stage and URL alias timestamps from the file are applied on
update; and export skips, with a warning, the gather queries plugins seed that
no import would accept (the OAuth and webhook admin lists, over tables gathers
may not read, and `scheduled_items`, whose filter field the validator refuses). `trovato_oauth2` had never run its fourth migration, which
its manifest did not list, so the `scopes` column every OAuth client lookup
selects did not exist and every lookup found no client; the manifest lists it
now.

Import runs in its own process and cannot reach a running server's caches, and
the server reads plugin status only at boot, so import now says to restart it.

The stage-aware config storage (`StageAwareConfigStorage`,
`AppState::config_storage_for_stage`) had no caller and is deleted.
Configuration is not staged.
