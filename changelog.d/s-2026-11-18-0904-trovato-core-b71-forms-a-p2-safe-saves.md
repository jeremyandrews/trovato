A save from either item form erased every field the form had not rendered:
both routes built the stored field map from the POST body alone, and
`Item::update` replaced the whole map, so a plugin written key, a key the type
does not declare and the values of a multi value field were lost by one save,
while a checked Boolean was stored as the string `"1"`. One submission
builder, `content::submission::build_item_fields`, now starts from the stored
fields and overwrites only the fields the form's server side state says it
rendered, coercing each by its field type: Booleans to JSON `true` and `false`,
numbers to numbers, multi value fields to arrays counted against their
cardinality, long text to its value and a permitted format.

`extract_content_fields` and the field loop in `ItemSubmission::from_form` are
gone. Each item form now saves its state under the `_form_build_id` it prints,
and a submission without a live state for the same user is refused as expired.
A migration rewrites Boolean strings already stored in items and revisions, so
gather filters on a Boolean match items saved from the admin form.
