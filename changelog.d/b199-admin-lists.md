Every admin listing now works the same way, and a Delete asks first even
with JavaScript off.

Before, a Delete button asked only through the script's `confirm()`. With
JavaScript off, or if the script failed to load, one press deleted at once.
A bulk delete asked "Delete selected items?" in English, without saying how
many. Now each destructive action posts to its usual address with
`?confirm`. A layer (`routes::admin_confirm`) keeps the submitted fields in
the session and shows a page of its own, `/admin/confirm`, the same with or
without JavaScript. The page:
- names what will happen ("Delete About us? This cannot be undone.");
- for a bulk delete, says how many items ("Delete 3 selected items?");
- offers the real button, which posts the same fields to the same address,
  so the handler and its permission and token checks are unchanged;
- offers Cancel, which goes back to the page the form was on.

A post whose form token does not verify never reaches the page: the handler
refuses it as before. A request without `?confirm` reaches the handler
directly, as before.

Listings could not be searched by text, most could not be sorted, several
showed only their first 100 rows with no way to reach the rest, and the
pager dropped any filter. Now one pattern (`routes::admin_list` and
`templates/admin/macros/list.html`) draws them:
- **Filter.** A GET form in a search landmark: a text filter plus the
  listing's own selects. It works without JavaScript, and a filtered address
  can be shared. It is on content, people, comments, files, media, gathers,
  menus, tiles, categories, tags, aliases, the site log and the audit log.
- **Sort.** Sortable columns have a button in their header, and the sorted
  header carries `aria-sort`. A sort keeps the filter and the filter keeps
  the sort. Only each listing's own allowed columns ever reach the SQL.
- **Count.** "N results" above the table, announced in a polite live region
  after a filter.
- **Pages.** Fifty rows a page, with pager links that keep the filter and
  sort, in a labelled navigation with the current page marked.
- **Empty.** A listing that is empty says what to do next. A filter that
  matches nothing says so and links back to the whole list.
- **Bulk.** The select-all box shows "mixed" when only some rows are
  checked, and a live region counts the selection.
- **Narrow screens.** Below 40rem a table lays each row out as a block, each
  cell under its column's name, so a listing no longer needs a sideways
  scroll at 320px.

Every admin table has a caption and scoped headers, and a test checks it.
The cron report's facts table gained the caption it lacked.

Removing a translation and removing a field's search weight ask first too.
