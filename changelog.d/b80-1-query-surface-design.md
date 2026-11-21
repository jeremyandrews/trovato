`docs/design/plugin-query-surface.md` records decision D-60, which supersedes
D-19: plugin reads of content go through a new `query` host interface compiled
by Gather for the request's viewer, stage and language; a plugin's own tables
gain conditions, upsert and bounded batch insert on `db`; translations are
written through `ItemService` from `item-api`; and `raw_sql` stays one flag for
what Gather will not compile, shown in the admin listing, the boot log and a
capability reference. Design only: no contract change in this entry.

A real-database test now pins the read gate `query-raw` relies on: PostgreSQL
runs the `DELETE` inside a `WITH ... AS (DELETE ...) SELECT` in an ordinary
transaction, so a first-keyword check could never hold, and refuses it with
SQLSTATE 25006 in the `BEGIN READ ONLY` transaction `query-raw` opens. BL-61 is
marked fixed by #125, which made that so.
