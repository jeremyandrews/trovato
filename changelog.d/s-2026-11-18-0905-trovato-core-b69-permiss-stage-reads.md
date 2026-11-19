Editors read from their stage; everyone else reads Live; a gather's total
counts only what its caller can see.

Which stages a read drew on was decided in three places, and none asked a
permission. `/admin/stage/switch` stored any string as the session's active
stage, unchecked; search and the alias fallback read that session value for
whoever held it; and the gather routes took `?stage=` from anyone, anonymous
visitors included, whose `total` then counted the published items on an
internal stage. One resolver, `routes::helpers::read_stages`, now answers every
read: Live, or the requested stage (a `?stage=` parameter or the session's
active stage) with Live beneath it for a holder of the new kernel permission
`use stages`. It is granted to no role; administrators hold it implicitly.
Without it a requested stage is ignored rather than refused, and the permission
is asked on every read, so revoking it takes effect on the next request. The
switch keeps its CSRF check, requires the permission (403 otherwise), and
answers 400 for a value that is not the id of a stage; reading the current
stage requires the permission too.

A gather's `total` was a SQL count that, for any signed-in viewer, included
rows the access pass then dropped, and its pages began at raw offsets, so a
page could repeat or skip visible rows. An Item gather now counts the
candidates the viewer can see, in one query up to the existing scan cap and
with the same `check_access` the page uses, and starts each page at the visible
row that opens it. The Pagefind index is documented as what it already was:
Live only.

Contract: additive for the permission. Frozen for the observable changes to
existing behaviour: `/admin/stage/switch` and `/admin/stage/current` answer a
non-administrator holder of `use stages` (an administrator still may use them)
and an unknown stage is a 400 where it was stored; `?stage=` without the
permission is ignored; gather totals change for signed-in viewers. No
host function, WIT type or SDK type changed. See
`docs/design/stage-workflow.md` §8.
