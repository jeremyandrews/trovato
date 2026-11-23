A category term reference field (`RecordReference("category_term")`) can now
be filled in by hand. Its widget, `static/js/record-ref.js`, asked
`/api/v1/items/autocomplete` for suggestions with the field's target type, and
that endpoint only ever searched the item table for items of that type. Terms
are not items, so for `category_term` it found nothing and the box offered
nothing, on every term field, owned vocabulary or not. Saving was unaffected.

The endpoint now answers a `category_term` target from the terms, in the same
`[{"id", "title"}]` shape and with the same `limit`, by a case-insensitive
prefix match on the term name through `CategoryService::search_tags`, which
reads the cached per-vocabulary term lists. The widget sends the field it is
filling in (`item_type`, `field`), the item when it edits one, and the form's
owner value for a field limited to the item's owner. A caller is offered terms
only if they may edit that field as the item form decides it: the type's
create permission or edit access to the item, plus the field's own view and
edit decisions. Anyone else, anonymous included, gets an empty list. A field
limited by `settings.vocabulary_owner` searches only the vocabularies its save
check accepts; the check and the search share one rule,
`OwnerLimit::admits`. An open term field searches the site-wide vocabularies
and never an owner's. Item autocomplete for every other target is unchanged.

Reopening an item also showed a blank box for a term field, because the edit
form looked titles up in the item table only. It now shows the term's name.
