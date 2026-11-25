A Gather filter, sort, exposed filter, contextual argument or option list on a
`fields.<name>` now matches a stored `{"value": "Barga"}` exactly as it matches
a stored `"Barga"`. The field-value module has owned a field's shapes since
B125: an edit form keeps a wrapped value wrapped, and importers and plugins
store wrapped values legitimately — some local databases carry upward of a
hundred `conference` items whose `field_city` is one. Gather never read that
shape. Its filter and sort expressions extracted `fields->>'city'` directly,
so a wrapped row's comparison text was the literal string
`{"value": "Barga"}`, never the city; `GatherService`'s two hand-written
option-list queries (`fetch_distinct_values`, `fetch_faceted_distinct_values`,
and the facet scope condition both build) had the same bug, so an exposed
filter's dropdown showed the wrapped JSON as one of its choices. A second
reader already existed and disagreed: the `item_search_vector` SQL function
unwraps the same shape for full-text indexing, so gather and search read one
row differently.

One function now owns reading a `fields` value as SQL, mirroring
`field_value.rs`'s `scalar_of`: the wrapper's payload when there is one,
otherwise the bare value. Every gather comparison, the sort expression and
both hand-written option-list queries call it; nothing else does. No index was
added — the GIN index on `fields` was never used for a text comparison to
begin with, and the new expression measures within noise of the old one.

Folded in: a filter value spelled as an unhyphenated or uppercase UUID now
compares as the exact text it was written as. `FilterValue`'s untagged enum
tries a `Uuid` variant before its string catch-all, and `uuid`'s deserializer
accepts the simple, braced, URN and uppercase spellings alongside the
canonical one — so any of those in a stored filter definition silently became
the canonical lowercase-hyphenated string before the comparison ran, which
never matched a JSONB text field storing one of the other spellings. The
`Uuid` variant now deserializes only from that canonical spelling; every other
spelling falls through to the string variant unchanged.
