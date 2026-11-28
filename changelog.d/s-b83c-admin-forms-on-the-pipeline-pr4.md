The structure and content admin screens build their forms through the one
form pipeline: categories and tags (site-wide and under a plugin's owner
routes), URL aliases and alias patterns, redirects, content types, their fields
and search settings, gather queries, tiles, menus, stages, files, comments,
the content list, translations and revisions. Each form is built by
`FormService::build` (its CSRF token in `_token`, `tap_form_alter`), drawn by
the admin form templates, and checked by `FormService::process` (the token,
the saved form state, `tap_form_validate`), and once a submission is saved
`tap_form_submit` hears of it. Before, these screens wrote their forms by hand
in Tera and checked a token and nothing else, so no plugin could alter,
validate or hear of them.

`routes::admin_form::check` now returns the saved form state, and the new
`admin_form::submitted` dispatches `tap_form_submit` with it. The admin forms
already on these helpers (access rules, text formats, appearance, languages,
content type search switches) call it too, so `tap_form_submit` runs for
them for the first time.

A POST to one of these screens is refused as an expired form unless it
carries the `_form_build_id` of the form the screen served to the same user:
a hand made post with a token alone changes nothing.

What an administrator sees change:

- A row's Delete on these lists is a link to a page that asks the old
  question and offers a red Delete button with Cancel beside it, where it was
  a button that posted to the generic confirmation page first. The generic
  confirmation page (`/admin/confirm`), still used by bulk deletes and by the
  screens not yet moved, is itself a pipeline form now, built under the id of
  the form that asked, with its Cancel link unchanged beside the button.
- A mistake on these forms is shown on the form, beside its field, where
  several were a bare error page before: a tile's machine name, item count or
  audience, a non-numeric weight on tiles, menus, stages and search weights,
  an alias pattern under a reserved prefix, a comment's subject, status or
  empty body, an alias or redirect with an empty path, an over-long alt text.
- The form renderer draws every field the same way, so the hand set field
  widths are gone, an optional select starts with "- Select -", a required one
  is marked "(required)" and has no empty choice, and a field's id is its
  name (the add-field form's fields are `label`, `name`, `field_type` and
  `category` now, and its machine-name script follows them).
- Rows of action buttons (comment moderation, gather cloning, alias pattern
  regeneration) belong to one form for the whole table rather than one each.
  Regenerating aliases asks one question for every content type.
- A few messages that carried a value ("A category with ID … already exists.")
  are written out in English, as the text format screen's already were.
