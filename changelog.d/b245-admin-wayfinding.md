The administration now shows a first time visitor where to go: each group
and its key pages carry an icon, the sidebar opens only where you are,
landing pages and the Dashboard say what waits, and each group's main action
is a button.

**Behavior change:** the sidebar opens only the group holding the current
page, and the Dashboard opens none. Before, every group was open unless the
viewer had collapsed it, so a first visit showed about 45 links in one
column. A group opened by hand is still remembered per user, now as the
groups opened (`expanded`) rather than the groups collapsed; the old saved
`collapsed` list is ignored, so every viewer starts at the new default, and
the current page's group is never saved. `POST /admin/preferences/nav` takes
one group and whether it is open (`{ "group": "reports", "open": true }`)
instead of the whole collapsed list, and writes it in one statement.

The admin has one icon set, drawn for Trovato: monoline like the wordmark,
decorative, never the only content of a link, and nothing that needs
mirroring right to left. Group headings in the sidebar carry their group's
icon; landing tiles and Dashboard cards show it in a clay on peach chip, a new
`--accent-tint` token held to 3:1.

Landing pages are a grid of tiles, and a tile shows its page's state where it
has one: unpublished items (for whoever may edit any content), comments
waiting for moderation, blocked accounts, errors in the last 24 hours, and
when cron last ran. Waiting work turns the line itself into a badge, so a
screen reader hears each count once. Each count is one indexed statement,
never cached. The Dashboard's group cards no longer repeat every link: each
is the group's chip, its title leading to its landing page, its status lines
and its main action, and a count a Dashboard panel already gives is not
repeated on a card; the Waiting for you and Recent errors panels now take
their comment and error sentences from the same status lines. Add content,
Add content type (newly in the registry) and Add user are filled buttons
beside their landing page's heading and on their Dashboard card.
