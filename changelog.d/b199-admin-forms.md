Every admin form now draws its fields one way. Before, about forty admin
pages wrote their forms by hand and the rest came from the Rust form builder,
and the two disagreed:
- A hand-written form showed one unlinked list of errors and marked no field
  as failed.
- Hints were tied to their fields on some pages and not on others.
- The builder marked a required field with a CSS asterisk that a screen
  reader does not read.
- A collapsible group opened only from a click handler on its legend, which
  the keyboard cannot reach.

Now one set of macros (`templates/admin/macros/form.html`) draws every admin
field. A form built in Rust for an `/admin` address renders through templates
that call the same macros, and a test proves a field comes out the same
either way. Another test fails on any admin template that draws a field by
hand. The front end's own form templates are unchanged.

The field pattern:
- The label sits above the field.
- Its hint sits under the label and is read as the field's description.
- "(required)" is part of the label's words.
- Choices are grouped under a legend.
- Advanced settings fold into native `details`.
- Person fields say what they hold (`autocomplete`).

A failed submit:
- The page title starts with "Error:".
- A "There is a problem" summary comes first, takes focus, and links to each
  failed field.
- Each failed field is marked invalid and carries its message.

Forms are validated on the server only, never as you type.

Saving and editing:
- On a long form the Save bar stays in view, without ever covering the field
  that has focus.
- Leaving a form with unsaved changes asks first, and only then.
- After a save, focus moves to the confirmation.
- Settings forms end with a short "What this changes:" line above Save.

The collapsible fieldset the form builder draws is native `details` on the
front end as well. Nothing in Trovato builds one today, so no page changes;
plain fieldsets render byte for byte as before.

Words built in Rust and shown on admin forms (field titles, hints, button
text, option labels) are now translated. In marker mode the untranslated-text
test can now see a Rust-built word printed without `t`, which it could not
before.

The words admin scripts show now come from a translated table the admin
shell renders. The compound field editor reads from it, and on the front end,
where there is no table, keeps its English.

On admin pages the media picker is now a modal dialog:
- Focus stays in it.
- Escape closes it and returns focus.
- Its filter, search and upload are labelled.
- Each file is a button named by its file name, and the result count is
  announced.

The front end keeps its existing picker.

The unused `templates/macros/forms.html` is removed.
