Scheduled publishing could only be set by hand: `trovato_scheduled_publishing`
read `field_publish_on` and `field_unpublish_on` as Unix seconds since B278
gave it precise timers, but no content type declared either field and the
item form had no input for them, so an editor's only way to schedule an item
was to add an integer field in the admin and type a timestamp into it. The
item add and edit form now carries a "Publishing schedule" group, for a
holder of `schedule publishing`: a publish date, an unpublish date, or both,
each read and shown in the editor's own time zone and converted to and from
UTC through Postgres, with the same refusals — a past date unless it is
unchanged, "unpublish on" before "publish on", scheduling a publish on an
item already published or an unpublish on one already unpublished, and a
local time a daylight-saving change skips over — enforced identically on the
JSON item API. Saving sets or moves the item's existing timer; clearing a
date cancels it; reopening the form shows what is scheduled. A user without
the permission never sees the group, and their save leaves any existing
schedule exactly as it was.

Writing the two dates from the item form needed one small kernel seam:
`tap_form_validate`'s input now carries the item being edited's id, and a new
`FormService::validate_fields` dispatches that same tap for the JSON item
API with a synthetic form id, so the one validation function a plugin writes
guards both paths rather than the form alone.
