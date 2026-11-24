A gather can count `?page=` from 0, the way Drupal's pager did, so every paged
link a migrated site published lands on the same items; a page past the end of
a listing is a 404; and a gather reached at an alias pages on that alias.

Drupal numbered its pager from 0 (`?page=1` was the second page) and Trovato's
gathers from 1, so every old paged link landed one page off. Which count a
listing uses cannot be guessed from the request, because `?page=1` means a
different page under each, so it is the gather's: `display.pager.zero_based`,
set by the gather that replaces a Drupal listing and offered in the gather
builder. A zero-based gather reads `?page=N` as page N+1 and writes its own
pager the same way, with no `?page=` on the first page, so its links and the old
site's agree. Every other gather is unchanged: `?page=N` is still page N, and
every link it has published still works. The pager template now links with
URLs the kernel computes (`pager.links`, `prev_url`, `next_url`, `first_url`,
`last_url`); `pager.pages` and `page_url_prefix` stay for themes written
against them, and pager links keep their filter parameters in a fixed order.

A page past the last answered 200 with an empty listing, which a search engine
reads as a soft 404. It is now a 404, except on the front page, where a 404
falls back to the stock front page.

A gather reached at a URL alias with no `canonical_url` paged to its internal
address: at `/blog`, the pager linked to `/gather/blog_listing?page=2`.
`render_query_html` built its link base from `canonical_url` or the gather id
and never looked at the address the visitor used. It now uses that address
when there is no canonical URL. No query was added: the pager settings come
from the in-memory gather registry.
