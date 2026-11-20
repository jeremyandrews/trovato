# One forms pipeline (B71)

Every content and profile form is built, altered, validated, saved and enhanced
by one kernel pipeline. This note fixes the shape before the code moves; the
Parts at the end say what lands when.

## Where it stood (re-verified on `main` at `4c75594`, after B68 to B70)

All nine findings in the B71 brief were still open. `FormService::build` and
`process` had no route caller; `build` started from an empty `Form` and handed
every plugin the same JSON, keeping the last answer. Content forms came from two
renderers (`content::form::FormBuilder` writing HTML strings, and
`templates/admin/content-form.html` branching per field type), the profile form
was hand written, a save rebuilt the field map from the POST body, nothing read
`cardinality`, `/system/ajax` was `administer site` only with no services,
`/item/{id}/edit` was a bare document with an inline `<style>` and no editor,
`kernel.wit` had no text format call, the sidebar was unfiltered (BL-117), the
"Add field" button was a GET to a POST-only route, and a checked Boolean was
stored as `"1"`.

What B68 to B70 changed that matters here: B69 put a stage selector (`_stage`)
on both item edit forms and decides moves in `ItemService::update`; it becomes
one more element the route adds to the base form. B70 is revision reads only.
Neither moved the plan.

## The pipeline

```
FormBuilder::build(type, item?, access) -> Form        (typed, no HTML)
route adds its own elements (alias, stage, log)  -> Form
FormService::build(form, session, state)          -> Form
    tap_form_alter, plugins in weight order, each given the previous output
    FormState { rendered_fields, item, type, permission, user } saved by form_build_id
ThemeEngine::render_form_with_errors(form, errors) -> HTML   (the one renderer)
    + the asset libraries the form needs, as <script src integrity> / <link>
POST: FormService::process(form_build_id, posted, session, state)
    CSRF (`_token`) -> built-in validation -> tap_form_validate
    -> submission builder -> FormResult::Valid(fields)
route saves through ItemService (the gates run there, unchanged)
FormService::submitted(form_id, values, state)  -> tap_form_submit, once
```

`FormBuilder` is the only place a `FieldType` becomes a widget; the submission
builder is the only place a posted value becomes a stored one. `content-form.html`
keeps the admin chrome (header, tabs) and prints the rendered form; its per type
branches and `FormBuilder`'s string rendering are deleted. Form ids are
`item_add_{type}`, `item_edit_{type}` (shared by the admin and `/item` routes)
and `user_profile_form`. The JSON `tap_form_alter` receives is `form::types::Form`
serialized: `elements` keyed by input name, each with `type`, `title`,
`default_value`, `required`, `weight`, `prefix`/`suffix` (plugin markup, emitted
raw, as today), `children`, `libraries` on the form. A plugin returns the whole
form; an empty or unparsable answer leaves the form as it was.

## Submission builder

```rust
pub fn build_item_fields(
    stored: Option<&serde_json::Map<String, Value>>,
    content_type: &ContentTypeDefinition,
    rendered: &[String],          // from FormState, never from the client
    posted: &PostedForm,          // every (name, value) pair, repeats kept
) -> Result<serde_json::Map<String, Value>, Vec<ValidationError>>
```

Start from the stored map; for each rendered field only, read its posted value
and coerce it by `FieldType`: Boolean to JSON `true`/`false` (an absent rendered
checkbox is `false`), Integer and Float to numbers (empty removes the key), Text,
Email and Date to strings, TextLong to `{value, format}` with the format checked
against the user's permitted formats, Blocks, Compound and PageBuilder parsed
from their JSON input. A field with `cardinality != 1` is posted as
`name[0]`, `name[1]`, …; it becomes an array of coerced values with empties
dropped, and more values than a positive cardinality is a field error. Every key
the form did not render (a field the user may not see, a plugin written key, a
key the type does not declare) survives untouched. `Item::update` keeps whole
value replace; its "merge" comment is corrected. JSON API updates keep their
semantics: top level keys are partial (absent title or status is kept), and a
`fields` object replaces the map, with fields the user may not edit copied back
by `ItemService::gate_field_writes`.

## Form state

`FormState.extra` carries `rendered_fields`, `item_id`, `item_type`, `user_id`
and `permission` (the check that let the user open the form), stored in
`form_state_cache` under the `form_build_id` the form prints. A POST whose
`_form_build_id` has no state (expired after six hours, or forged) re-renders
the form with the submitted values and "This form has expired"; nothing is
saved. A state whose `user_id` is not the requester's is the same.

## AJAX

`/system/ajax` stays the one AJAX route. It requires `X-CSRF-Token` (a rotating
token the form prints in `_ajax_token`; each response sets the next one, because
kernel tokens are single use), loads the state, and requires the requester to be
its `user_id` and to hold its `permission` (for an edit form also `edit` access
to the item, asked again). No `administer site`. It dispatches with a full
services `RequestState`. "Add another" is the built-in trigger `add_{field}`:
the server builds the next widget with `FormBuilder` and appends it; there is no
client side clone.

## `visible_when`

`FieldDefinition.visible_when: Option<VisibleWhen { field, equals }>`, serde
default, so existing config loads. The renderer prints
`data-visible-when-field`/`-equals` on the field's wrapper; a static
`static/js/form-states.js` hides and shows. Without JavaScript every field is
shown. On the server the builder evaluates the condition against the posted (or
stored) value of `field`: when false the field's value is not saved (stored kept)
and its required check is skipped.

## Asset library and SRI

`form::assets` defines libraries by name (`block_editor`, `compound_field`,
`file_upload`, `record_ref`, `ai_assist`, `form_states`, `form_ajax`), each a
list of stylesheets and scripts. The renderer attaches a library whenever the
form contains its widget, on any page; a plugin may add a name to
`form.libraries`, never a URL. Choice: **SRI on the existing jsDelivr origin**.
The eight pinned files (Editor.js 2.31.5 and seven tools, 330 KB together) are
immutable at their version URLs, so a `sha384` hash and `crossorigin="anonymous"`
pin each byte for byte; `script-src` keeps exactly the origins it has, and no
second copy of 330 KB of Apache 2.0 code enters the repository with its licence
notices and update chore. Vendoring would let `cdn.jsdelivr.net` leave the CSP
later, which is BL-68's business, not this one's.

## Text format host call (WIT)

```wit
interface text-format {
    /// Filter `text` through the named format, as the kernel does on render.
    /// Errors when the format is unknown or the acting user may not use it.
    filter-text: func(text: string, format: string) -> result<string, string>;
}
```

It calls `FilterPipeline::for_format` after the same permission rule the forms
use (`plain_text` always, `use filtered_html`, `use full_html`).
`KERNEL_API_VERSION` gains a minor version.

## Parts

1. This note. 2. Safe saves: the submission builder on both existing routes,
Boolean as JSON. 3. One pipeline: typed `FormBuilder`, chained alter, one
renderer, libraries, four forms moved, B36. 4. AJAX permission and services,
add another, `visible_when`, record autocomplete. 5. The WIT call. 6. BL-117:
the viewer into `render_admin_template`, sidebar filtered from one table.
