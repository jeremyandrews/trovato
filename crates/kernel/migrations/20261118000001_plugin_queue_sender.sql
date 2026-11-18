-- B68: shared plugin queues.
--
-- A job pushed with `push-to` onto a queue another plugin owns is stored under
-- the owner (plugin_name), so the owner's drain, claim and width apply
-- unchanged, and this column records the plugin that pushed it. NULL for every
-- existing row and for the owner's own pushes, which the worker keeps
-- receiving as their bare payload.
ALTER TABLE plugin_queue ADD COLUMN IF NOT EXISTS sender TEXT;
