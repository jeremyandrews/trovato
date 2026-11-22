Search result snippets show their text once, with the matched terms
highlighted, whatever the stored body holds. A body written as HTML came out as
`&lt;p&gt;Tom &amp;amp;amp; Jerry …` on the results page, so a site had to undo
the escaping in its own template. The snippet was escaped twice: the search
query HTML-escaped the title and body before `ts_headline` (the XSS-3 fix), and
the route escaped the whole headline again in `sanitize_snippet` before the
template printed it `| safe`. Neither step stripped the markup, so the tags the
first escape had turned into text were escaped a second time and shown. The
route's escape also restored any `<mark>` it found, so a body holding a literal
`<mark>` got a highlight the kernel never made.

There is now one rule, in `crate::search::snippet`. The query strips HTML tags
from the stored title and body before highlighting, not from the highlighted
result, and `ts_headline` marks matches with two fixed private-use characters
that the query also removes from the source, so stored content cannot forge
one. `snippet::render` then splits the headline at those markers, takes the text
of each piece with the kernel's existing extractor (the ammonia-based one the
meta description and the Pagefind export use), escapes it exactly once, and
turns only the markers into `<mark>`. `SearchService::search` is the one place a
snippet is built, `templates/search.html` the one place it is printed unescaped,
and `sanitize_snippet` is gone. The JSON endpoints (`/api/search`,
`/api/v1/search`) carry the same snippet. No query was added.

The blog listing's teaser had the same fault from the other side: it printed the
stored HTML body through `truncate`, so autoescape showed its tags and turned a
stored `&amp;` into a visible `&amp;`. It now goes through a new `plain_text`
Tera filter, the same text extraction, so the teaser is text escaped once.
