A gather feed now gives a reader what it needs to follow a site: each entry
names its author and its taxonomy terms, keeps one id for life, carries a
plain-text summary, and every gather feed is served as Atom 1.0 as well as RSS
2.0, with `ETag` and `Last-Modified` so a repeat poll is answered `304`.

The feed renderer wrote title, link, description and date, nothing more. The
`guid` was the link, so moving a post to a new URL alias made every reader show
it again as new; the id is now `urn:uuid:<item id>`. The description read a
top-level `summary` or the body off the row, but a gather that selects the whole
item nests its fields under `fields`, so most entries had no description, and
the ones that did carried the stored HTML whole. The summary is now the item's
summary field when it has one, otherwise its body stripped of markup and cut at
a word boundary, extracted and escaped once by the rule B123 gave search
snippets. A new `full_content` option adds the body, rendered through its text
format, as `content:encoded` and Atom `<content type="html">`. Authors (`dc:creator`,
never an email address) and terms (`<category>`) are loaded in one batched
query each; the alias lookup the feed did for itself is gone, since the gather
service already puts the alias on every row.

The Atom document is at the RSS path's `.atom` sibling (`/blog/feed.xml` and
`/blog/feed.atom`), derived so that every existing feed gets it with no config
change, and both are advertised in the page head. Every absolute URL comes
from `SITE_URL`, and the kernel now logs one warning at startup when `SITE_URL`
is unset or a loopback host. The query still runs as the requesting viewer on
every request, so an anonymous feed is `Cache-Control: public, max-age=300` and
one fetched with a signed-in session is `private`.
