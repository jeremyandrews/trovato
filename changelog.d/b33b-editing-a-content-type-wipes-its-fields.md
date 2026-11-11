Saving a content type's edit form no longer deletes the type's fields. The
type metadata writer, `ContentTypeRegistry::update`, wrote the form's three
settings keys (`title_label`, `published_default`, `revision_default`) over the
whole `item_type.settings` column, and the field list lives in that column
under `fields`, so every save removed every field declaration along with any
other settings key the type carried. The cache hid it: `update` rebuilt its
cache entry from the previous cache entry rather than from the row, so the
admin kept showing the fields until the next reload from the database, or the
next restart, and then the type had none. Item data was never touched, but
nothing declared its keys any more. `update` now merges the keys it is given
into the row's current settings in one SQL statement, never writes `fields`,
and rebuilds the cache from the row the statement returns; the field writers
merge by the same rule, so neither writer can drop what the other stored.

The add form could overwrite an existing type the same way. It asked the cache
whether a machine name was taken, and `ContentTypeRegistry::create` upserted,
so a row this process had not cached yet (written by a second server, or by a
config import before the next reload) had its label, description and whole
settings, fields included, replaced by the form's. `create` now inserts only
when no row has the name, decided by the database, and refuses otherwise with
`CreateContentTypeError::AlreadyExists`, which the form shows as the existing
"already exists" message. Overwriting a whole definition on purpose is
`ContentTypeRegistry::replace`, a fixture and import tool the test suite uses
to reset its shared types.

Types whose fields were already lost are not repaired by this change: the
declarations cannot be reconstructed from the type row.
