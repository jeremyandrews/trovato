# Scheduled Publishing

An editor who holds `schedule publishing` can set a future publish date, an
unpublish date, or both, on the normal item add and edit form. No separate
screen and no field to add to the content type by hand: the two inputs are
part of the "Publishing schedule" group near the bottom of the form,
alongside Promoted, Sticky and the other publishing options.

## Scheduling an item

1. Open an item's add or edit form (`/item/add/{type}` or `/item/{id}/edit`,
   and the equivalent admin content forms).
2. Under "Publishing schedule", set "Publish on", "Unpublish on", or both.
   Each input is a date and a time.
3. Save. The item goes live, or comes down, at that exact second — through
   the same precise timer every other scheduled change in Trovato uses (see
   [`docs/plugin-timers.md`](plugin-timers.md)), with the cron sweep as a
   backstop if a timer is ever missed.

Reopening the form shows the stored schedule back, in the same time zone,
and a line under the group — "Scheduled to publish on …" or "Scheduled to
unpublish on …" — says what is set without having to decode the inputs.

Clearing an input and saving removes that date and cancels its timer.

## Time zone

Both inputs are read and shown in the editor's own account time zone (set
on the user's profile), labelled on each field so it is never ambiguous
whose clock it is — "Publish on (Europe/Rome)", for instance. A user with no
time zone set reads and writes in UTC. There is no site-wide default time
zone to fall back to before UTC.

A local time that does not exist, because clocks spring forward past it for
daylight saving, is refused with a message naming the problem rather than
silently shifted by an hour. A local time that occurs twice, because clocks
fall back through it, is read as the earlier of the two real instants.

## What gets refused

- **A date in the past** — unless it is exactly what is already stored, so
  reopening and resaving an item whose scheduled time passed a moment ago
  does not fail.
- **"Unpublish on" at or before "Publish on"**, once both are set.
- **"Publish on" set on an item that is already published** — there is
  nothing to schedule; unpublish it first, or clear the date.
- **"Unpublish on" set on an item that is unpublished with no "Publish on"
  pending** — same reasoning, the other direction.

Each refusal names the field and explains why, on the form exactly as any
other field error does. The JSON item API enforces the same rules against
`fields.field_publish_on` and `fields.field_unpublish_on` (plain Unix
seconds there, no time zone conversion), so a script cannot save a schedule
the form would have refused.

## Permission

Seeing and changing the schedule needs `schedule publishing`. A user without
it who saves the form — editing the title, say — never sees the group, and
the item's existing schedule, if any, is left exactly as it was.

## Storage

The two dates are stored as Unix seconds in the item's own `fields`,
`field_publish_on` and `field_unpublish_on` — the same shape
`trovato_scheduled_publishing`'s timers and cron sweep already read. No
content type needs to declare either field.
