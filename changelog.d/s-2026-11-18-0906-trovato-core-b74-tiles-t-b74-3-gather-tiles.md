Fixed BL-99: a `gather_query` tile renders the first rows of its gather server side through `elements/tile--gather_query.html`, overridable per tile, with a link to the gather's own page.
The tile's gather runs once, as the viewer, in the page's language, through `execute_query_only`, the path the gather's JSON and feed use; `execute_query_only` takes an optional page size.
A `gather_query` tile's config takes `items`, the number of rows from 1 to 50 (default 5); the tile form and `config import` refuse any other count, and export carries it.
**Behaviour change:** a `gather_query` tile renders its rows instead of an empty `<div class="tile-gather" data-query-id>`.
**Behaviour change:** a `gather_query` tile whose gather the viewer may not run, or that names no gather, is left off the page; the second logs a warning.
