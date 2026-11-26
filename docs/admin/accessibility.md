# Accessibility of the administration

The target is WCAG 2.2 AA on every admin page, by keyboard and by screen
reader, in light and dark, right to left, at 320 CSS pixels wide and at 400%
zoom. This page says how that is checked and what a screen reader user hears.

## What checks it

| Check | Where | Runs |
|---|---|---|
| Contrast of every text pair (4.5:1) and every boundary and the focus ring (3:1), computed from the token values in both themes | `crates/kernel/src/routes/admin_design_tests.rs` | `cargo test` |
| Logical properties only, in `admin.css` and in any inline style left in an admin template; focus ring at least 2px | same | `cargo test` |
| Every admin route listed once, in the group the registry gives it | same, against `docs/admin/inventory.md` | `cargo test` |
| Page anatomy: skip link first, one `main#main`, banner, `nav` "Administration", search landmark, one `h1` that leads the `<title>`, breadcrumb ending at the page with `aria-current`, no tablists | `crates/kernel/tests/admin_anatomy_test.rs` | `cargo test` |
| No untranslated interface text on any admin page | `crates/kernel/tests/admin_i18n_test.rs` (see [translation.md](translation.md)) | `cargo test` |
| axe-core, WCAG 2.0/2.1/2.2 A and AA, on every GET page in the inventory, in four modes: light, dark, 320 px, and the right-to-left pseudo locale; no sideways page scroll at 320 px | `tests/admin-a11y/specs/axe.spec.mjs` | CI job "Admin accessibility" |
| Keyboard: skip link, sidebar disclosures by Enter and Space (and remembered), the narrow-screen menu (Escape closes it and returns focus), search by Tab and Enter, a visible focus ring on everything focusable | `tests/admin-a11y/specs/keyboard.spec.mjs` | same |
| The accessibility tree of each page type, and of the open command palette | `tests/admin-a11y/specs/aria.spec.mjs` | same |
| The dashboard's panels (regions named by their headings, state in words), "Who can do this?" (last in `main`, answers for its page) and the accessibility report | `tests/admin-a11y/specs/dashboard.spec.mjs` | CI job "Admin accessibility" |
| The command palette: modal, combobox and grouped listbox, Up/Down/Enter/Escape, focus returned, `/` ignored in text fields, the live count, the same results as the page, axe on the open palette | `tests/admin-a11y/specs/palette.spec.mjs` (also run under WebKit, Safari's engine, by hand) | CI job "Admin accessibility" |

Running the browser checks locally, against a site started with
`TROVATO_PSEUDO_LOCALES=1` and seeded with `trovato generate`:

```sh
cd tests/admin-a11y
npm ci && npx playwright install chromium
ADMIN_A11Y_BASE=http://localhost:3000 ADMIN_A11Y_USER=gen_admin ADMIN_A11Y_PASSWORD=… npm test
npm run screenshots                                   # docs/images/b199/
ADMIN_A11Y_RECORD_ARIA=1 npx playwright test --project=light specs/aria.spec.mjs   # screen-reader/
```

A parameterised route (an edit page) is opened at an address found by
following links from its listing. Only the routes in `EXPECTED_UNSEEDED`
(`tests/admin-a11y/routes.mjs`) may go unopened, each with the reason the
generated site has nothing to open; any other route with no address fails, so a
listing that stops linking to its pages fails the job rather than dropping
them. A plugin page may answer 404 only when the plugin is not one the site
installs (`scripts/seed-plugins.txt`, read by both the seed script and the
checks).

## What a screen reader user hears

From the accessibility trees in [screen-reader/](screen-reader/), recorded by
`aria.spec.mjs` from Chromium's tree. These are the trees VoiceOver and NVDA
read from; no live VoiceOver session was recorded for this part (see "Not
done" below).

**Every page** starts with "Skip to main content, link". Next comes the banner: the
Trovato link to the Dashboard, then a search landmark holding "Search the
administration, search field" and a "Search" button, then "View site" and
"Your preferences". Next is the "Administration" navigation: the Dashboard
link, then seven level-2 headings, each holding a collapsible button named for
its group ("Content, collapsed" or "expanded"), followed by the group's links:
Overview, then its pages. The current page is announced as "current page".
Main follows. On every page except the Dashboard it starts with the
"Breadcrumb" navigation, for example "Administration, link; Content, link;
Content", where the last item is the current page and not a link. Then comes
the page's one level-1 heading and its intro sentence.

- **Dashboard**: "Dashboard, heading level 1", the intro, then the panels,
  each a region named by its level-2 heading: "Your recent content", "Waiting
  for you", "Scheduled tasks", "Recent errors". Each starts with a sentence
  that gives the state and its numbers in words ("Nothing is waiting for
  you.", "1 error in the last 24 hours."), then a list of links. Then "Areas you
  may use, heading level 2". Each group is a region named for it, with a
  level-3 heading, its one-line description and a list of links. A "Create"
  region lists one link per content type. "You" says "Signed in as
  gen_admin" and offers Your preferences and a Log out button.
- **Group landing page** (People): "People, heading level 1", what the group
  is for, then a list. Each page in the list is a link with its description
  under it. Only pages the listener may open are there.
- **List** (Content; the pattern is [design.md section 9](design.md#9-table-pattern)):
  the heading, the intro, "Moderated content" and "Add content" links, then
  a search landmark named "Filter content". It holds a "Title contains"
  search field, "Type" and "Status" comboboxes and a "Filter" button. Next
  comes the count, for example "12 results". After a filter or sort the same
  words are announced as a status. The bulk action follows: "Bulk action"
  and "Target stage" comboboxes, an "Apply" button and a status that counts
  the selection ("Selected: 3"). The table is a scrollable region named
  "Content items", announced with its caption. Its first column header is a
  "Select all items" checkbox, announced "mixed" when only some rows are
  checked; every row checkbox is named for its item. Sortable column
  headers hold a button ("Title, button"), and the sorted one is announced
  "sorted ascending" or "descending". Each row starts with the item's title
  as a row header. Its actions name the item: "Edit Post 3", "Delete Post
  3". Last comes the "Pages of content" navigation, where the current page
  is "current page".
- **Confirmation** (after Delete, with or without JavaScript): "Delete Post
  3, heading level 1", a sentence saying to check what will happen, then the
  question, "Delete Post 3? This cannot be undone.". It is followed by a
  "Delete Post 3" button and a "Cancel" link back to the listing. A bulk
  delete says how many: "Delete 2 selected items? This cannot be undone.".
- **Form** (Add stage): the heading, the intro, then each field by its
  label with "(required)" spoken as part of the name ("Machine name
  (required), edit text"). The hint is read as the field's description. The
  checkbox is "Default stage for new content", with its explanation as the
  description. "Create stage" is a button and "Cancel" a link.
- **Form with errors** (Add role, saved empty; the form pattern is
  [design.md section 8](design.md#8-form-pattern)): the page title starts
  "Error:". Focus is on an alert, "There is a problem", which is read at once
  and lists each problem as a link ("Role name is required."). Following a
  link moves focus to the field, which is announced as invalid, with the
  problem as its description ("Error: Role name is required."), after its
  hint.
- **Form built in Rust** (Add content, Add stage): read exactly like a
  hand-written form. Every field comes from the same macros, so a listener
  cannot tell the two kinds apart. Radio and checkbox groups are groups named
  by their legend. Collapsed settings are a "disclosure triangle" button
  (`summary`), opened with Enter or Space.
- **Settings form** (Site settings): the fieldsets are groups named by their
  legends. Just before Save, a paragraph starts "What this changes:".
- **Media picker** (a file field's "Browse media"): a modal dialog named
  "Choose media". Its controls are a labelled Type combobox, a "Search files"
  search field and a "Choose files to upload" file chooser. Each file is a
  toggle button named by its file name, pressed when chosen. After each
  search the number of files shown is announced as a status. Escape closes
  the dialog and focus returns to "Browse media".
- **Your preferences**: a group "Colour theme" with three radio buttons,
  System, Light and Dark, and the hint as their description. Next is the
  "Administration language" combobox, where each language is named in its own
  language and marked with its language code. A "Search shortcuts" group
  holds the checkbox "Open search with /" and the combobox "Another key to
  open search", each with a hint. A sentence says what the
  change affects, and a "Save preferences" button follows. A separate "Sign
  out" section holds the Log out button. After a save the page says "Done:
  Your preferences were saved." as a status message, which is announced
  without moving focus.
- **Search results**: "Search results for “content”, heading level 1", a
  second "Search again" search landmark with the query filled in, then
  "N results" as a status message. Then one region per kind, named by its
  level 2 heading (Pages, Settings, Content, People, Plugins, Log
  messages). Each lists its results as links, each followed by where it
  lives or what it is ("Setting on Site settings", "page, published",
  "Plugin, enabled"). A kind with more matches ends with a link such as
  "All matches in Content".
- **Who can do this?** (the link at the foot of each page, for someone who
  may change permissions): "Who can open Menus, heading level 1", then the
  region "What it needs" ("Menus needs the permission “administer site”."),
  then "Roles that may open it", a table captioned "Roles that may open
  Menus" with one row per role, headed by the role's name as a link. Tree:
  [screen-reader/who-can.yml](screen-reader/who-can.yml).
- **Accessibility report** (Reports): three regions, "Images without a text
  alternative" (a sentence with the number, then a captioned table whose
  row headers are links such as "Add a text alternative to photo.jpg"),
  "Your settings" and "How this administration is checked". Tree:
  [screen-reader/a11y-report.yml](screen-reader/a11y-report.yml).
- **Command palette** (Quick search, `/`): a modal dialog "Quick search"
  holding a search landmark: "Quick search, heading level 2", then
  "Search the administration, combobox, collapsed" and the instruction
  "Up and Down move through the results, Enter opens one, Escape closes."
  as its description. After typing, the combobox is expanded, and
  "18 results" is announced politely. The listbox "Results" holds a group
  per kind, named "Pages", "Settings" and so on. Down makes an option active:
  the tree gives its name, for example "Content Administration › Content",
  and its selected state. Focus stays in the combobox throughout. Tree:
  [screen-reader/palette.yml](screen-reader/palette.yml).

Messages say what they are in words as well as colour: "Done:", "Warning:"
and "Error:". Errors are alerts, read as soon as they appear; the rest are
status messages.

## Keyboard

Nothing opens on hover. Tab order follows reading order: skip link, banner
(search, account links), sidebar, then the page. A sidebar group's button
toggles with Enter or Space, and its state is remembered for the next page.
The group holding the current page is always open. Below 48rem the sidebar
sits behind a "Menu" button. Opening it does not move focus, so the sidebar
is the next stop after the banner. Escape closes it and returns focus to
the button. Every focusable element shows a 3px ring 2px outside it, in
the focus colour, and in system colours under forced colours.

On a form, the fields come in reading order and Save follows them. The
"Cancel" link comes after Save. On a form taller than the window, the Save
bar stays at the bottom of the screen, and the page scrolls a focused field
clear of it. Leaving a form with unsaved changes asks first (the browser's
own question), and only then.

Search: in the banner, type and press Enter for the results page, which
works without JavaScript. With JavaScript, "Quick search" opens the command
palette. So does `/` while focus is not in a text field, unless you turn it
off in Your preferences ("Open search with /"). There you can also add
Control+K (Command+K on a Mac) or Alt+K (Option+K), which work in text fields
too. In the palette, type to search. Down and Up move through the results
and wrap at the ends. Enter opens the active result, or the full results
page when none is active. Escape or Close shuts it, and focus returns to
where it was.

On every page, for someone who may change permissions, the last stop in
the page content is "Who can do this?". Enter opens the answer for that page.

On a listing, Tab reaches the filter fields, then the column headers' sort
buttons (Enter sorts), then each row's checkbox and actions. Space checks a
box; checking "Select all items" checks every row. Delete leads to a page
whose button does it and whose "Cancel" link goes back. Nothing deletes
without that second press.

## Not done yet

- **No live VoiceOver session.** No recorded VoiceOver or Guidepup session:
  driving VoiceOver needs the macOS accessibility permissions granted
  interactively, which an unattended run cannot do. The trees above are what
  VoiceOver reads, but its exact wording and verbosity were not heard. For
  the command palette this matters most: `aria-activedescendant` is checked
  in Chromium and in WebKit (Safari's engine), but not yet heard through
  VoiceOver in Safari. If VoiceOver does not follow it, the fallback in
  [design.md section 10](design.md#10-search-pattern) applies.
- **Pages never opened.** The parameterised pages with nothing seeded to open
  (an access rule, a tile, a menu link, a record, an AI provider, a
  translation, a deletable text format) were not opened by the browser checks.
  The marker-mode and anatomy tests open what the test fixtures create.
