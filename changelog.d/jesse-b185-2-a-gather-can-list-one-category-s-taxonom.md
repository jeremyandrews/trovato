A gather definition can now list the terms of one category (vocabulary),
filtered, sorted and paged like any other gather, through a new kernel-owned
`category_term` lightweight-record type over `category_tag`. A site writes
`"record_type": "category_term"` with a bound `"category"` filter (the
vocabulary's machine name) the same way it would name a plugin's record type,
and gets back rows carrying each term's `id`, `title` (its name), `weight` and
`url` — the stock gather tile and row templates render them with no new
template, the same way they already render an item listing.

`category_term` is registered directly by `RecordTypeRegistry::with_builtin`
rather than through a plugin manifest, since `category_tag` is kernel-owned and
no plugin's effective DB allowlist should have to name it. A gather naming
`category_term` with no bound `category` filter is refused, both when it is
saved and on every run (an exposed filter left unanswered is caught too,
since it is dropped before the refusal runs) — `category_tag` holds every
vocabulary's terms in one shared table, so an unconstrained listing would mix
them all together. A term's row carries a `url`: its `url_alias` entry when one
is set, `/term/{id}` otherwise, the same convention an item's `/item/{id}`
uses. Terms carry no language overlay yet (there is no `category_tag`
translation table): a gather in a non-default language renders a term's
default label, the same gap term pages already have.

Every existing `item` and record-type gather's emitted SQL is unchanged; the
new behaviour is additive and gated entirely on `record_type: "category_term"`.
