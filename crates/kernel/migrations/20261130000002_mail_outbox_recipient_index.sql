-- no-transaction
-- BG18 part 2: the mail log's recipient filter, "what happened to mail to
-- this address", reads that address's rows only. Built concurrently; a failed
-- build leaves an INVALID index under this name: DROP INDEX CONCURRENTLY
-- mail_outbox_recipient_newest, then rerun.
CREATE INDEX CONCURRENTLY mail_outbox_recipient_newest ON mail_outbox (lower(recipient), id DESC);
