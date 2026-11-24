-- no-transaction
-- BG18 part 2: the mail log's status filter and per status counts read
-- (status, id) instead of walking the whole outbox. Built concurrently so
-- sending is not blocked; a failed build leaves an INVALID index under this
-- name: DROP INDEX CONCURRENTLY mail_outbox_status_newest, then rerun.
CREATE INDEX CONCURRENTLY mail_outbox_status_newest ON mail_outbox (status, id DESC);
