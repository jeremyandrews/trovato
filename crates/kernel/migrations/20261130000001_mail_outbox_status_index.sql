-- no-transaction
-- BG18 part 2: the network mail log's status filter and per status counts
-- read (status, id) rather than the table. Built concurrently so
-- sending is not blocked; a failed build leaves an INVALID index under this
-- name: DROP INDEX CONCURRENTLY mail_outbox_status_newest, then rerun.
CREATE INDEX CONCURRENTLY mail_outbox_status_newest ON mail_outbox (status, id DESC);
