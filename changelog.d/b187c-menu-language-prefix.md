On a page served in a language other than the site default, every menu link to
a page that exists in that language now keeps the language prefix, and the link
for the page being read is marked active, as it already was in the default
language. `localize_menu_links` prefixed a link only when it resolved to an item
with a translation, so a link to a gather page (`/recipes`), a term, the front
page or a plugin route kept its default-language address on `/es/recipes`: the
click left the language. And the active check compared each link with the
address as asked for, `/es/recipes`, so a link either side of the prefix never
matched, and an unprefixed link never could. The rule for a link existing in a
language is now the one the router already serves by: an item link exists when
the item has a translation (unchanged), and any other local path exists in
every language unless language negotiation passes it through untouched
(`/static`, `/api`, the health probes, `/install`), which both now ask through
one function, `is_served_per_language`. External URLs are left alone. Active is
decided with the language prefix removed from both the link and the requested
address, so an untranslated item's unprefixed link is active on its page read
under `/es`, and the existing rule that a section link is active on the pages
under it is kept.
