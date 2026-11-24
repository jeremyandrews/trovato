Old links that carry a query string reach their page, and a feed's item count
and full text are set in the gather builder without the save losing the feed.

A Drupal site without clean URLs published every link as `/?q=node/96`. On
Trovato that was the front page, because `check_redirect` matched the path
alone and skipped the site root. A request for the root, or for `/index.php`,
whose `q` names a site path now follows that path's redirect in one hop when
there is one, and otherwise goes to the path itself, keeping its other
parameters. A `q` that could leave the site (`//host`, a scheme, a backslash, a
control character) is ignored. And a redirect dropped the request's query
string, so `/old?page=2` landed on `/new`. It now lands on `/new?page=2`. Only
requests with `?q=` at the root take the new branch, so no query was added
anywhere else.

The gather builder edits a feed's number of entries and whether each entry
carries the full text (`display.feed.items` and `full_content`, from B127),
which are the RSS publishing settings a Drupal site had. Before this, a save
through the builder erased every display key the builder does not show: it
posted only its own keys, and the save handler stored exactly what was posted,
so saving a gather there dropped its feed, its routes, header and footer. The
save now lays the submitted display over the stored one: a key the form does
not send keeps its value, and a key sent as `null` is cleared.
