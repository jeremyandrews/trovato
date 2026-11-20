Every page and item template gets `viewer`: `{id, authenticated, roles}`, the id a string (`null` when anonymous) and the role names sorted; nothing else about the viewer reaches a template.
Role names are loaded and cached by `PermissionService` with the permissions, in one cache entry, and carried on `UserContext`; a warm cache costs no new query.
The item template gets `plugin_output`, every `tap_item_view` output on its own; `children` is unchanged, the field dump followed by that same output.
The item route hands the viewer it loaded to the new `inject_site_context_for`, so an item page loads its viewer once.
**Behaviour change:** fixed B63, an item page printing "body format: filtered_html": the field dump showed a text field's sibling `<name>_format` key as a field of its own, and now skips it when the type declares `<name>` as text.
