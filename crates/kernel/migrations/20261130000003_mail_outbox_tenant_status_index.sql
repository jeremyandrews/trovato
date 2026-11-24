-- no-transaction
-- BG18 part 2: one blog's per status counts (an index only scan) and its
-- status filter, which the blog owner page always applies. Built
-- concurrently; a failed build leaves an INVALID index under this name:
-- DROP INDEX CONCURRENTLY mail_outbox_tenant_status_newest, then rerun.
CREATE INDEX CONCURRENTLY mail_outbox_tenant_status_newest ON mail_outbox (tenant_id, status, id DESC);
