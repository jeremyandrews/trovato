The dashboard says what needs you, every admin page can say who may open
it, and Reports has an accessibility report.

**Dashboard panels.** Above the area cards, the dashboard now shows:
- **Your recent content**: how many items you wrote and how many are not
  published, and the last five, each linking to its edit form, or to the
  item when that is all you may do.
- **Waiting for you**: drafts to review and comments waiting for approval.
- **Scheduled tasks**: whether the last cron run worked, failed or was
  skipped, and plugin queue jobs that stopped after repeated failures.
- **Recent errors**: errors the site log recorded in the last 24 hours, the
  newest five, each linking to the log filtered to it.

Each panel is a section with a heading, and gives its state in a sentence,
with numbers as words. A panel is shown only to someone who may open the
page it is about, by the same rule as the sidebar.

**"Who can do this?"** Each admin page ends with this link for someone who
may change permissions. The page it opens (`/admin/people/who-can`) names
the permission the page needs and lists the roles that hold it, with how
many people each has. It works the answer out from the page's address
using the registry's own gate, so the address cannot change it. Without an
address it lists what opens every admin page, kernel and plugin.

**Accessibility report** (`/admin/reports/accessibility`, under Reports).
It lists the images that have no text alternative, each linking to where
one is set. It also shows your own settings (colour theme, search
shortcuts) and how the administration is checked.

On a moderated type, the item form's current state ("Current state:
Draft") is now translated like the rest of the form.

The plugin guide now says where one admin link declaration appears: the
sidebar, the group's landing page, the dashboard, search, breadcrumbs and
"Who can do this?".
