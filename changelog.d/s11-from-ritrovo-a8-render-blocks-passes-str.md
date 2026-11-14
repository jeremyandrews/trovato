The `render_blocks` template filter now escapes a plain string instead of
passing it through, and a gather that projects the whole `fields` column now
drops the fields `tap_field_access` denies, as a `SELECT item.*` gather always
did.

The filter's output is meant to be used with `| safe`. An array of blocks went
through the block renderer, which sanitizes it, but a string value was returned
unchanged, so the filter trusted a string nothing had sanitized. Any write path
that stored a Blocks field as a string (a plugin's raw SQL, a config import, a
TextLong value rendered with this filter) could put live markup on a page. A
string is now HTML-escaped and renders as text; content that is meant to be
markup belongs to the `markdown` or `text_format` filter. Block rendering is
unchanged. The tutorial templates' comments now say what the filter guarantees.

Gather field access mapped only `fields.<name>` projections back to the item
field they read, so a projection named exactly `fields`, labelled or not, mapped
to nothing and its nested object came back whole, every denied field included,
to anyone allowed to run the gather. That projection is now filtered key by key
exactly like the star case, on every gather path (named, ad hoc, includes and
semantic search). `fields.<name>` projections are filtered as before.
