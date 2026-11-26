Path aliases resolve for every character set, letter case and language, and a
new category tag gets the address of its page.

The alias fallback looked a path up exactly as the request carried it, which
broke three kinds of address. A browser sends `quiche-mediterráneo-profundo` as
`quiche-mediterr%C3%A1neo-profundo`, so every alias with a character outside
ASCII was a 404. A link in another letter case (`/BLOG/TILLICUM`, common in old
sites whose database compared aliases without case) was a 404. And under a
language prefix the lookup fell back to any language's alias, so
`/es/recipes/borscht-with-pork-ribs` served the Spanish page at a second address
beside its own `/es/recipes/borscht-con-costillas-de-cerdo`.

`UrlAlias::resolve_path` now matches the path as requested and percent-decoded
(an alias stored either way resolves, so nothing that resolved before stops),
and failing that, ignoring case through a new index on `lower(alias)`; the same
statement returns the page's own alias in the requested language. The fallback
serves an exact match, redirects a case-only match to the stored spelling, and
redirects another language's alias to the page's own alias in the prefix's
language, all with a permanent redirect that keeps the query string. A page with
no alias in that language is still served under the prefix by the alias it
has. It is still one query for an aliased page, as before.

A category tag created without a slug had no page address, because pathauto
made aliases for items only and a term page is found by its slug
(`/category/tags/{slug}`). `Tag::create` now gives such a tag a slug from its
name, unique in its category (`pv2-term`, `pv2-term-1`); a slug given is kept.
