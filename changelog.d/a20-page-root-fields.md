A plugin can declare page level fields for page builder pages, beside the
built in title, and a page can use them to keep, hide or replace a sitewide
part such as a footer call to action. Before, a page's `root` was read into a
struct with a `title` and nothing else: any other key the editor stored there
was dropped when the page rendered, never validated when it was saved, and
seen by no template, and the editor's page settings form could only offer the
title because nothing declared anything more.

Root fields are declared in `components/<name>.root.toml`, next to a plugin's
components, as a list of `[[fields]]` in the component field format with the
same eleven types, limits and checks, and optionally the `classes` a root
select may add. Two enabled plugins declaring one root field name stop the
server at boot naming both, as a duplicate component does; `title`, `props`
and `variant_classes` are reserved, and a default the field's own rules refuse
is an error at boot. `GET /api/v1/page-builder/components` serves them as
`root_fields`, and the editor builds its page settings form from them with the
same control per type as a component field.

Every save checks the root like a component's props, with the error at
`field_layout.root.<field>` (a 422 from the JSON API). A root key no enabled
plugin declares is refused on save; a stored page is never validated to
render, so one carrying such a key renders as it did, without passing it on.
The kernel reads the root in either shape Puck writes, flat or under
`root.props`. The editor stores it flat and leaves out keys no enabled plugin
declares.

Page templates, the item template and every tile template on the same request
receive `page_root`: the title, each declared field normalized to its type
(with `<name>_html` for rich text) or its declared default when the page has
no value, and `variant_classes`. A page with no page builder content gets the
defaults. A `custom_html` tile is now rendered through
`elements/tile--custom_html--<machine name>` or `elements/tile--custom_html`
when a template of that name exists, with `tile`, `body` and `page_root`, and a
tile whose template renders only whitespace is left off the page. A page
saved before this change validates and renders byte for byte as it did. See
"Page root fields" in `docs/plugin-development.md`.
