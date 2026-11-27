A comment body and a custom HTML tile body are now checked against the
writer's permitted text formats before they are saved, the same check an
item field already ran (B268). Comment create and edit, and custom HTML tile
create and edit, used to store whatever format was posted with no check at
all, so a writer could save a body in a format they may not use, for example
full HTML, from the form, the API or `trovato import`. The check item fields
already ran has moved into `text_format::check_permitted`, one function the
item field path, the comment service and the tile service all call, so
routes, the API and the importer all get it from one place.

That function also tells apart two refusals that used to share one message.
A format that does not exist at all now answers "the X text format does not
exist", where before it answered "you may not use the X text format", the
same wording used for a format that exists but is not among the writer's
permitted ones. `trovato import` reused that check, so a typo in a migration
file used to read like a permission problem; it now names the missing format
correctly, for an item field, a comment and a tile alike.
