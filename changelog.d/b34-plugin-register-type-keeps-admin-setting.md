Restarting the kernel no longer resets a plugin-declared content type such as
`blog`. Plugin registration, which runs on every boot, wrote the plugin's
declared field list over the whole `item_type.settings` column through a
whole-row upsert, so each boot dropped every settings key an admin had saved
(`published_default`, `revision_default` and any other), every field an admin
had added, and the admin's title label. Registration now merges instead, in one
statement: the plugin's declared fields win by name, replacing the stored field
of the same name whole and keeping its position, and declared fields the type
lacks are appended; every other field and every other settings key stays as it
was, and the title label is kept unless the plugin declares one. A field the
plugin stops declaring also stays, since without a provenance marker it cannot
be told apart from one an admin added, and keeping it leaves stored item data
with a definition. Registering the same declaration again writes identical
settings, and the registry now caches the row the database holds rather than
the bare declaration. `ItemType::upsert`, used by config import and
`ContentTypeRegistry::replace`, still replaces the whole column.
