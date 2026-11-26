Admin search finds settings, content, people, plugins and log messages, not
only pages, and a command palette opens it from anywhere.

Before, `/admin/search` matched the titles of the sidebar's pages and nothing
else. Now one search (`routes::admin_search_index`) finds, each kind under
its own heading:
- **Pages**: by title, description, group and synonyms, in English and in
  the viewer's admin language.
- **Settings**: a single setting field by its label, hint or synonyms,
  linking to the field itself (`/admin/config/site#site_slogan`). A test
  opens each settings form and checks that every field it links to is
  there.
- **Content**: item titles, the eight most recently changed, with a link to
  every match in the content listing.
- **People**: account names and email addresses.
- **Plugins**: the pages enabled plugins declare, and the plugins page's
  rows, linking to the plugin's row.
- **Log messages**: the newest site log messages that match.

Each kind is shown only to someone who may open the page it links to, by
the same rule the sidebar uses, and every page keeps its own permission
check. The queries run live, so something deleted or a plugin turned off
is gone from the next search.

The banner has a new "Quick search" button that opens a command palette.
It shows only when JavaScript runs; without it, the banner's search form
leads to the same results. The palette:
- is a modal dialog with a combobox and a list of results grouped by kind;
- asks `GET /admin/search/suggest`, which runs the same search as the
  results page, so the two never disagree;
- moves through results with Up and Down, opens one with Enter, closes
  with Escape and returns focus;
- announces the number of results politely.

`/` opens it when focus is not in a text field. Under "Search shortcuts" in
Your preferences, each person can turn `/` off and add Control+K (Command+K
on a Mac) or Alt+K (Option+K).
