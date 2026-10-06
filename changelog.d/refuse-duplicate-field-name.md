Adding a field to a content type now refuses a machine name the type already
has, and a database that already holds duplicates is repaired on migration.

`ContentTypeRegistry::add_field` pushed the new definition onto
`settings->fields` without looking at what was already in the list, and neither
caller checked either: the admin form and the AJAX callback behind the same
button validated the label, the machine name's syntax and the field type, and
nothing about collision. So a type accumulated two or three definitions of one
field, one per submission. It is not a cosmetic duplicate — at three copies of
`search_test_field` on `page`, the content translation form rendered none of the
type's fields at all. The check now lives in the registry rather than at either
route, so every path that adds a field inherits it, and both routes turn the
typed refusal into a message that names the machine name that collided.

Two defects in the same function are fixed with it. `add_field` built
`{"fields": ...}` and wrote it over the whole `settings` column while
`persist_fields`, three lines below and used by field edit and field delete,
merges into the existing object for the stated reason that non-field keys have
to survive; so adding any field through the admin silently dropped
`title_label`, `published_default` and `revision_default` from the type. It now
persists through that same merge. And an unrecognised `field_type` string fell
through a `match` to `Text`, so a typo or a stale option in the template stored
a field of the wrong type without a word; it is now refused and named.

The `20261006000001_dedupe_item_type_fields` migration repairs rows already
written, keeping the first definition of each name and dropping later copies.
The first is the one kept because it is the one the type has been running with.
No item data is touched: `item.fields` and `item_revision.fields` are JSONB
objects keyed by field name, so one key holds one value however many times the
type declared the field, and a redundant definition cannot own a value of its
own. The migration is idempotent, and only rewrites rows that actually hold a
duplicate.

The manage-fields template rendered neither the `errors` nor the `values` the
route had been passing it since before this change, so every validation refusal
on that screen — an empty label, a malformed machine name, a missing field type
— came back as an unchanged page with no message and the typed values gone. It
now shows the errors and keeps the submitted values.
