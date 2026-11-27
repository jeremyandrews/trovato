Admin listings are quicker to read at a glance. Each row action shows only its
verb ("Edit", "Delete", "Approve"), so the actions column stays narrow and the
eye runs down the titles, while its accessible name is still the verb and the
row's name ("Edit About us"), built with `aria-labelledby` from the action's own
id and an element in the row header that holds only the row's name. The new
`list::row_action` and `list::row_button` macros, and `list::delete_button`'s
new `text`, `id` and `row_id` arguments, draw every row action this way; the
translations and revisions of an item keep full sentences, because their row's
name is only a language code or a date. A row's state is one `list::status`
pill, quiet unless the row needs someone: a blocked account, spam or an error
in the log reads as a problem; unpublished, pending, temporary and warnings ask
for attention; everything else is plain. `admin_list::ListState` is the one
place a state's word and kind are set, and the per-listing status colours are
gone. In Files an image row shows a small thumbnail from the image styles
plugin's `thumbnail` style. The content, files and comments listings read their
accounts through the user cache in one `find_many` call instead of one query per
row.

**Behavior change: dates.** Every admin date is now written in the viewer's own
timezone (`users.timezone`, UTC when it is unset) and language, as "Today,
14:05", "Yesterday, 09:30" or "3 Nov 2026, 05:20", inside a `time` element
whose `datetime` holds the full time with its offset. Before, each page wrote
its own format, always in UTC. Templates use the new `admin_time` and
`admin_time_text` functions, and a `UiMessage` takes a timestamp with
`.time(name, ts)`, so the Dashboard panels and the Cron status line are
formatted for the page that shows them. The cron sentences were reworded so a
date never follows "at", and a date in a right to left language is isolated so
its digits keep their order. Form inputs that take a date are still UTC.

**Behavior change: sort order.** Text in admin listings now sorts in natural
order, ignoring case, with numbers by value and an accented letter beside its
plain one: "Post 2" before "Post 10", "eclair" beside "Éclair". A migration
creates the ICU collation `trovato_natural` (`und-u-kn-true`), which the sort
expressions name (`admin_list::natural!`) without changing any column, and an
index on `lower(name) COLLATE trovato_natural` keeps the people listing's first
page an index scan. Listings sorted in memory use a key that gives the same
order. Titles that differ only in punctuation such as `_` or `~` can still
order differently between the two.
