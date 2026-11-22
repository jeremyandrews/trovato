A plugin can add page builder components without touching the kernel. A
component is a declaration, `components/<name>.component.toml`, a Tera template
in the plugin's `templates/`, and optionally a stylesheet in its `static/`. At
boot the kernel builds one registry from its standard components and those of
every enabled plugin, and the editor, the server-side validation and the
renderer all read from it. The Puck editor no longer carries a hard-coded list:
it builds its component list and every field form from
`GET /api/v1/page-builder/components`, and shows a component it has no React
preview for (any plugin's) by asking the kernel to render it, through the new
`POST /api/v1/page-builder/preview`, with the page's own template, sanitizer and
checks. The kernel's thirteen standard components are declared in the same
format, so they are the examples. See "Page builder components" in
`docs/plugin-development.md`.

The format is TOML, like `.info.toml`, read with unknown keys refused, so a
misspelt limit is an error at boot rather than a rule that never applies. A
file that does not parse, a missing template or stylesheet, or a type name two
plugins both declare stops the server, naming the file and both plugins. Field
types are `text`, `rich_text` (Markdown, rendered and sanitized by the kernel),
`number`, `heading_level`, `url`, `image` (alt text required unless marked
decorative), `link`, `select`, `boolean`, `list` and `reference` (an item or a
gather query, resolved as the viewer when the page is viewed). Every rule is
checked on the server on every create and update that goes through the item
service; a page that breaks one is refused with an error naming the component
and the field (`field_layout.content[2].cards[1].title: is required`), shown on
the item form and returned as a 422 by the JSON API. Before, nothing checked a
saved page on the server: the registry had a `validate_page` (component type,
heading level, one image's alt text) that no write path called, and only the
editor knew the rest. A component's template receives its declared fields only, each
normalized to its type, so a value the declaration does not name, an unsafe URL
or a select value outside its options never reaches it.

The page builder's sanitizer no longer allows the `style` attribute, and no
component writes one from a prop. The four standard components that did now
take a declared variant instead: Hero, Call to Action and Section have a
`background` select (none, light, dark, brand, adding a `pb-bg--*` class), and
Columns has `gapSize` (default, none, small, large). The free-text
`backgroundColor` and `gap` fields are hidden in the editor and no longer
rendered; a stored page keeps the values, still validates, and renders without
them. The standard Hero's background image, which was a `background-image`
style, is an `<img>` behind the content. A page saved by 0.106 that does not use
those three props renders byte for byte as it did, and a test holds that. See
UPGRADING.md.

`/static/css/page-builder.css` is now linked on a page that has page builder
content; nothing linked it before, so the standard components rendered
unstyled outside the editor. A plugin component's stylesheets are linked the
same way, each once and only when the component is on the page, from
`/static/plugins/<plugin>/`, where the kernel now serves an enabled plugin's
`static/` directory. An enabled plugin's `templates/` directory is on the
template search path at the lowest precedence.

A Hero saved without a `variant` failed to render ("Variable `variant` not
found"): the template defaulted the class but compared the raw value further
down. It now defaults it once.

The release image ships only the plugins it builds (BL-122). The Dockerfile
made a directory under `/app/plugins` for every directory in `plugins/`, so the
in-tree test and reference plugins were discovered with a manifest and no
module behind them. It now assembles a directory only for each built module,
and copies its `components/`, `templates/` and `static/` along with its
manifest and migrations.

The editor's source moves from a single file to a small TypeScript project in
`plugins/trovato_page_builder/editor/`, built with esbuild into the committed
`static/js/page-builder-editor.js` and `static/css/page-builder-editor.css`,
with Vitest tests of the stored-page conversion and the registry-built
configuration.
