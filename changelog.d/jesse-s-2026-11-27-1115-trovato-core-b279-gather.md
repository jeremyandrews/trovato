The admin gather builder knew only a handful of filter operators and treated
every filter value as plain text, so opening a stored gather and pressing
Save without touching anything could silently rewrite it: an operator the
builder's menu did not list (`starts_with`, `has_any_tag`, `is_null` and
others) fell back to whatever its `<select>` happened to default to, and a
value naming the page (`{"current_item_field": "field_tags"}`, a URL
argument) rendered as `[object Object]` and saved as that literal string. The
builder now offers every operator the gather engine accepts for the base
table, grouped and labelled in plain words, and each filter row gets a value
source: a fixed value, this item's id, a field of this item, a URL argument,
the current user, the current time, or the current date. A filter the
builder does not understand (a plugin's own `custom` operator) is kept as
stored and shown read only, never rewritten. A fixed value spelled exactly
like one of the contextual keywords (`current_item`, `current_user`,
`current_time`, `current_date`) is refused, since the engine would read it
back as that contextual value rather than as literal text. Saving an
is_null or is_not_null filter through the builder also used to fail outright
("missing field value"), since the script omitted the value key entirely
instead of writing it as null; it now writes null, as the engine expects.
