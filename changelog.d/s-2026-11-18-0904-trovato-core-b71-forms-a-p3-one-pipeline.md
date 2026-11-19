No route called `FormService::build` or `process`, so `tap_form_alter`,
`tap_form_validate` and `tap_form_submit` never fired for the item, admin
content or profile forms, each of which was hand written HTML with its own
widget mapping; and `build` gave every plugin the original form and kept the
last answer, so two alterations could not compose. `FormBuilder` now emits a
`Form`, every one of those forms goes through `FormService::build` (chained
alters, form state) and `process` (CSRF, `tap_form_validate` with field
errors) and fires `tap_form_submit` once after a save, and one renderer draws
them with the scripts each widget needs attached as asset libraries.

The `/item/{id}/edit` form now carries the block editor for a Blocks field and
renders every value of a multi value field plus one empty slot, inside the site
theme without inline styles. AI Assist buttons arrive through
`tap_form_alter` with their script. The field list's "Add field" button links
to the form on the page instead of the POST-only route (B36).
