Before 1.0 froze the plugin SDK, a migrating site's most common field kinds had
no kind of their own: a link, a fixed choice list and a phone number all fell
back to plain text, with nothing to check that a link was not `javascript:`,
that a list value was one of the choices an editor meant to allow, or that a
phone number held only digits and the punctuation people actually type.

`FieldType` gains three variants: `Link` (`{"uri", "title"?, "new_tab"?}`, its
`uri` an absolute `http`/`https` URL or a local path), `List` (one key of a
field's `settings.allowed_values`, single or multiple by the field's ordinary
cardinality), and `Telephone` (`{"value", "e164"?}`, its E.164 form filled in
on write when the number has a recognisable country prefix). Each is checked on
every write path a field reaches: the admin form, the JSON item API, a
plugin's `save-item`, config import and `trovato import`; each renders safely
on the item page (a link's scheme is re-checked at render time, never trusted
from storage); each is exported and reimported with its content type; and each
indexes its text part (a link's title, a list's label) in both search
backends. A `List` field is also filterable in Gather, exposed as a select of
its allowed values rather than a free text box, with the multi value case
reusing the existing JSONB array containment condition.
