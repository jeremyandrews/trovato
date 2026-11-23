Site search works for the query a visitor types, shows its results with
JavaScript on or off, and writes them once.

A query holding `&`, markup or a stray quote answered 500. The search built its
`to_tsquery` text by hand, appending `:*` to each whitespace-separated word, so
punctuation in the query was tsquery syntax. The words now go to
`websearch_to_tsquery`, which never fails to parse: every word must match,
"a quoted phrase" matches as a phrase, `or` and a leading `-` work as they did
on Drupal. Markup in the query is reduced to its text first, the way the
indexed text is, so `<b>wscci` searches for `wscci`. A query left with no word
to search for (`a`, `it's`, `&`) says so on the page instead of only "no
results". The count query reports that, so no query was added.

With JavaScript on, a visitor never saw a result: `search-init.js` hid the
server-rendered results as soon as `scolta.js` loaded, before knowing whether
the Pagefind index existed, and on a site with none built
`/pagefind/pagefind.js` answered 404 and left an empty box. The server results
are now hidden only once Pagefind has loaded; until then, and for good if it
cannot, they are the page. `templates/search.html` wrote those results twice,
in a `<noscript>` and again in the fallback block, both visible with JavaScript
off; they are written once.

On the Pagefind path every excerpt was escaped twice. Pagefind's `excerpt` is
HTML, its text already escaped with `<mark>` around the matches, and
`scolta.js` ran `escapeHtml(stripHtml(…))` over it, so "Tom &amp; Jerry"
showed its entity. `scolta.js` now reads every Pagefind value as HTML into
text once, and escapes once when it writes, highlighting on the text rather
than on escaped HTML; its `escapeHtml` escapes quotes too, since it fills
attribute values. Tests for it run on Node's own runner in a new
`Static JavaScript` CI job.

Searching asks for a permission now, `search content`, Drupal's name for it, so
a site can withhold search from a role. A migration grants it to every role
holding `access content`, so an existing site searches as before.
