Ten read paths that returned item data now decide what the caller may see
through `ItemService::check_access` and the field-access seam, the way the REST,
SSR, gather and search paths already do. Each of them loaded items with
`Item`/`ItemService::load` or with raw SQL of its own and never passed the
result through either seam, so its visibility rules were whatever that one
query happened to say, and they had drifted from the rest of the kernel.

The revision list at `/item/{id}/revisions` asked for a login and nothing else,
so the titles and log messages of every draft on the site were readable by
anyone with an account; it now answers to `view` and then `edit`, the bar the
revert button on the same page already applied. The RecordReference autocomplete
endpoint took no session at all and matched on `status = 1`, which is not a
stage filter, so an anonymous caller could enumerate the titles of published
items on internal stages a prefix at a time. The item page rendered the title of
every item a reference field pointed at and of every item pointing back, and the
edit form prefilled a reference target's title, all from unchecked loads: the
viewer had passed `check_access` for the page's own item, which says nothing
about its neighbours. Posting a comment loaded the parent item only to notify
its author, so `post comments` was also a way to confirm that a draft exists,
and reading a comment by id checked the parent item but never the comment's own
status, so a held, unpublished or spam comment — which the listing endpoint
filters out — could be read one id at a time. The MCP `list_items` tool forced a
published default and returned the page unfiltered, so it handed over items that
`get_item`, in the same file and through the proper seam, reported as missing.
The four content-translation screens required `translate content` and nothing
more, and put the item with every field into the template context. The AI chat's
RAG context and the static Pagefind index each went from SQL straight to
formatted text, the first into a model prompt and the second into a file any
visitor downloads, with no decision at either tier. And an AI assistant
conversation could be opened on any item whose type the scope listed, viewable
or not, after which the scope plugin was asked for context about it.

Nothing about the access model itself changed: no new policy, no change to the
published fast path, and the field-access default stays fail-open. A denied item
is still indistinguishable from a missing one wherever the surface already
answered 404 for missing, and every response shape and status code a permitted
caller sees is what it was.

Two supporting changes come with it. `CronService` now holds the item service,
wired from `AppState`, because the Pagefind export has to be filtered as the
anonymous visitor who downloads the index — with the anonymous role's real
permissions, not an empty set — and a cron service built without one skips the
rebuild rather than publishing an unfiltered index. The export's "which items
and which text" step is a function of its own so it can be tested without a
Pagefind CLI on the path.

Holding the revision list to `view` and then `edit` turned up a second defect
behind it, and that one is fixed here too: `Item::get_revisions` and
`Item::get_revision` each selected eight named columns, and `ItemRevision` grew
`change_summary` and `ai_generated` two migrations later without either list
being updated. `FromRow` wants a column per field, so both queries failed at
runtime for every caller, which is why `/item/{id}/revisions` answered 500 to
everybody and the revert button behind it could not work. Both lists now name
every column the struct declares.
