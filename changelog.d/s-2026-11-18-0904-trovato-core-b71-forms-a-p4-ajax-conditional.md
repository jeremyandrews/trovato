Form AJAX worked for administrators only, and not even for them from a
browser: `/system/ajax` required `administer site`, dispatched
`tap_form_ajax` without services, and the client sent no CSRF header while
the route required one, so "Add another" and every plugin trigger failed. The
route now takes a rotating per form token in `X-CSRF-Token`, recalls the form
state for the caller and re-checks that they may open the form, runs
`tap_form_ajax` with the host API, and builds "Add another" on the server for
any editor.

Fields can now carry `settings.visible_when: {field, equals}`. The form hides such a
field while the condition is false (`static/js/form-states.js` keeps it
current), and a save neither stores its posted value nor requires it. The
record reference autocomplete is attached to the item form on every route.
