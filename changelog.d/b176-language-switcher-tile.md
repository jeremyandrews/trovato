A "Language switcher" tile: an administrator places it in any region from the
tile admin, like any other tile, and it links each enabled language to this
same page in that language, marks the current one, and renders nothing at all
on a site with only one language — there being nothing to switch to.

An item page already knew its own translations, through
`available_translations`, but that fact reached the page's `<head>` as
`hreflang` alternates and nowhere else: nothing on the page itself offered a
reader the click. Tiles render earlier in the request than the item route
computed that list, so the item route now computes it first and the tile
layer reads it back out of the page context, rather than duplicating the
lookup. A page type with no translation list of its own — a term page, a
gather listing, anything else — offers each other language's front page
instead, the same fallback Drupal's own switcher block uses for a page with
no translation of itself.

The tile's own markup is a `nav` labelled by the tile's title, one link per
language carrying `hreflang` and `lang`, and `aria-current="true"` on the
language being read. The template is a stock one,
`elements/tile--language_switcher.html`, so a theme overrides it the way any
tile template is overridden.
