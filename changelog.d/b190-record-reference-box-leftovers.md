Three leftovers from the `category_term` `RecordReference` work (B137, #236).

The term box's placeholder read "Search category_term..." — and, for an item
reference field, "Search blog_post..." and the like — because the template
built it straight from the field's target type machine name. The placeholder
is now a human label the route resolves when the form is built: "terms" for
a category term field, the target content type's own label for an item
field, run through the same locale lookup a translated interface string
would use, and falling back to a plain "Search..." when no label is known.

A field limited by `settings.vocabulary_owner` offered the item's *stored*
owner's terms even after the editor changed or cleared the owner field on the
open form, because the widget only sent the owner value when it was
non-empty: an empty owner field looked, to the endpoint, exactly like a
request that said nothing about an owner at all, so it fell back to what was
stored. Saving then refused the item (no owner) or the term (`TermsNotOwned`,
wrong owner) with a term the box had just offered. The widget now sends the
owner field's current value whenever the field has one, blank or not, and the
endpoint tells "empty" from "absent" apart: an owner changed on the form
offers the new owner's terms, one cleared offers none and shows a short
"Choose the owner first" hint instead of the stale list. The endpoint still
resolves the owner with the one rule the save path already enforced,
`OwnerLimit::admits` — nothing about what a write accepts changed.

A form rendered before this field information existed (or before B137 at
all) could only have gotten there as an already open browser tab; nothing the
kernel stores server side — the form state table, the AJAX "add another"
rebuild — carries pre-rendered widget markup forward across a deploy, so
there was no stale state to migrate. Reloading the page is enough.
