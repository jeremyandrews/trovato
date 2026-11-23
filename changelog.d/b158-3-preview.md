The item forms preview: a Preview button beside Save renders the submission as
the item page would, above the form shown again with what was posted, and
saves nothing. A content type's "Preview before submitting" setting
(`preview_mode`) makes it optional (the default, as in Drupal), disabled, or
required, in which case the form offers Save only under a preview and a save
posted without one is refused. The forms had Save and nothing else, so an
editor could not see an item before it was stored, and a migrated site whose
types required a preview lost the step.

One path for all four forms (`/item/add`, `/item/{id}/edit` and the two admin
forms): `item_form::build` adds the button and withholds Save, the form state
of a form built under a preview records it, `item_form::process` refuses a
required type's save without it, and `item_form::render_preview` renders the
fields through the item page's own renderer, cut to what the user may see,
with the title escaped. A preview runs the same validation as a save and
nothing after it: no write, no tap, no lock change.
