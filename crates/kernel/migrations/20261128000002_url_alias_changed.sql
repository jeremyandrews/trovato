-- B214: a feed's channel link is the outbound alias of `/gather/{query_id}`
-- when the gather has neither an argument route nor a `canonical_url`, and
-- editing that alias rewrites its `url_alias` row in place (`UrlAlias::update`),
-- leaving `created` as it was. Nothing on the row moved, so a feed's
-- `Last-Modified` could not follow an alias edit. `changed` tracks that, set on
-- insert and on every update, backfilled from `created` for existing rows so
-- an alias nobody has touched since this migration reads as unmodified.
--
-- No `SET NOT NULL`: the previous release's `INSERT INTO url_alias` (the
-- model, the config-storage importer, two plugin seed migrations) names no
-- `changed` column, and would violate it during a rolling upgrade. The
-- `DEFAULT` keeps such a row non-null without one; application code always
-- names the column explicitly once this release is running everywhere.

ALTER TABLE url_alias ADD COLUMN changed BIGINT DEFAULT EXTRACT(EPOCH FROM NOW())::bigint;
UPDATE url_alias SET changed = created WHERE changed IS NULL;
