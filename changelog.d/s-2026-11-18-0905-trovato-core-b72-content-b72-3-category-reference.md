**API change.** A field can hold tags of one category, the item page resolves
them, and a field's `cardinality` holds on every write path. `FieldType` had no
category kind (`RecordReference` targets item types), so Ritrovo declared its
topics as something else: nothing checked that a stored uuid named a tag of the
right category, no form offered a tag picker, and the item route resolved
references to items but never to tags. Separately, `cardinality` was counted
only by the item form's submission builder, so the item API, a plugin's
`save-item`, revert and `config import` stored any number of values.

`trovato_sdk::types::FieldType::CategoryReference(category_id)` is the new
kind. Its value is a JSON array of tag uuid strings, whatever the cardinality,
the shape gather's `has_tag`, `has_any_tag`, `has_all_tags` and
`has_tag_or_descendants` already read. The add field screen offers it with a
category select it requires, and the edit screen can point it at another
category. The item form renders it as the existing reference widget, one tag
per slot on the repeating mechanism, showing labels and posting uuids, backed by
`GET /api/v1/tags/autocomplete?category=<id>&q=<text>` beside the items
endpoint, from the category's cached tag list; the one
autocomplete script now takes its endpoint from the widget and binds slots added
by "Add another". The endpoint answers as the viewer: an owned vocabulary the
viewer may not read offers nothing, by the rule every category read applies.

One check, `content::field_values`, counts each written field's values against
its cardinality and checks every `CategoryReference` value against the field's
category, with one query for all the tags a write names. The item service runs
it on create, update and revert, so it stands behind the admin and item forms,
the item API (422, naming each value) and `save-item` (the new
`ERR_ITEM_INVALID_FIELDS`, -65); the form uses its count rule for the field
error; and `config import` runs it in its validation pass, so a refused value
imports nothing. Stored data is not rewritten, and a field an update leaves as
it was is not judged again.

The item page has `referenced_tags`, keyed by field name: each tag's id, label,
slug, the URL of the gather route whose `tag_category` parameter lists its
category (null when none does), and its ancestors root first, from one query
for the tags and one recursive query for all their ancestors, leaving out the
tags of an owned vocabulary the viewer may not read. The item template
variables are documented, with an example, in `docs/design/Design-Render-Theme.md`.

**Plugin-facing change in 0.107.0: `FieldType` is `#[non_exhaustive]`.** A new
field kind had no way to be additive: `FieldType` was an exhaustive enum, so
adding `CategoryReference` broke every plugin with an exhaustive `match` on it,
and every kind added after 1.0 would have needed a major version. With the
attribute, the next kind is an additive change. A plugin that matches
`FieldType` needs a `_` arm for the kinds it does not handle; one that only
constructs field types, or already has a `_` arm, is unaffected. Both this and
`CategoryReference` ship in the unreleased 0.107.0 and change no released
version, so the plugin API version does not move for them. An SDK test now
matches every kind exhaustively, so adding one points at the kernel's `_` arms
(`content::field_value`, `content::form`) that must learn it.
