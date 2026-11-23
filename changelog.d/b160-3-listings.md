Term pages are titled by their term, the site's items per page reaches the
listings that do not set their own, and an item's views can be counted and
shown as "N reads".

A gather route whose path names a tag (`/recipe-category/{slug}`) titled its
page with the gather's label, so every term page was called "Recipe category".
The route already looked the tag up to filter by it, then dropped it. It now
keeps the tag it found: the page title, the breadcrumb and the listing heading
are the term's name, and the term reaches the templates as `term`. The shipped
blog listing template heads `/blog/tag/{tag}` with the tag's name instead of
"Blog". No query was added.

The "Items per page" on `/admin/config/site` was stored and read by nothing:
every gather had its own `items_per_page`, defaulting to 10 when its definition
left the key out. `display.items_per_page` is now optional. A gather that names
no size pages by the site's setting, so a migrated listing that Drupal paged by
"posts per page" follows it. The gather service caches the setting and drops it
on a `SiteConfig` cache generation bump, so a listing pays no query for it while
the cache is warm. A gather that names its own size is unchanged. The gather
builder leaves the field blank to mean the site's setting.

Items had no view counter (Drupal's statistics "N reads"). "Count content
views" on `/admin/config/site`, off by default, counts each item view in a new
`item_counter` table: one upsert per view, which also returns the total. The
total is shown under the item's title to roles holding the new
`view post access counter` permission. While counting is off, an item view
costs no query for it, because the setting is cached the same way.
