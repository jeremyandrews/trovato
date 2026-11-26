# The Trovato administration: design

This is the specification the admin is built to. It was written before the
code of the redesign (B199) and is kept true as the parts land. Where the code
and this file disagree, one of them is a bug.

The admin is a workspace for people who keep a site running: editors, site
builders, moderators, administrators. It should feel calm, ordered and
predictable, work the same for a keyboard, a pointer or a screen reader, work
without JavaScript, and read naturally in any language and either direction.

It is its own thing. It is not modelled on Drupal's Seven, Claro or Gin, on
WordPress, or on any dashboard kit, and it copies no icon, layout or
illustration from one.

## 1. Information architecture

### Groups

Every admin page belongs to exactly one of seven task groups, plus the
Dashboard. The group is a property of the page in the registry
(section 2), not of its URL: URLs are kept as they were, so no bookmark, link
or test breaks, and a page whose URL reads `/admin/structure/stages` can sit in
Configuration because that is where someone looking for it goes.

| Group | Slug | What sits in it |
|---|---|---|
| Content | `content` | Content, Add content, Moderated content, Comments, Files, Media |
| Structure | `structure` | Content types (with their fields and search settings), Record types, Gathers, Menus, Tiles, Categories (and their tags), URL aliases, URL alias patterns (pathauto) |
| Appearance | `appearance` | Themes |
| People | `people` | Users (with each user's sessions), Add user, Roles, Permissions, Access rules |
| Extend | `extend` | Plugins, AI providers, AI budgets, AI chat, AI assistant, and every plugin-declared page whose section names no other group |
| Configuration | `config` | Site settings, Text formats, Search settings, Stages, Account recovery |
| Reports | `reports` | Recent log messages, Audit log, Cron, Search report, Accessibility |

The Dashboard (`/admin`) and Your admin preferences (`/admin/preferences`) are
personal pages and belong to no group.

Adjustments from the starting brief, with the reason for each:

- **Revisions** and **translation** of an item have no listing page of their
  own: a revision list is per item (`/item/{id}/revisions`), and so is a
  translation list (`/admin/content/{id}/translate`). They are reached as local
  tasks of the item (View, Edit, Translate, Revisions), so Content holds their
  entry points through the content list rather than as sidebar links that could
  only lead nowhere. There is no translation queue in the kernel today; when a
  plugin adds one it declares it into Content.
- **Languages** have no kernel admin page. The `trovato_locale` plugin declares
  its own pages into Configuration.
- **Embedding status** (`/admin/embed/status`, `/admin/embed/backfill`) is two
  JSON endpoints with no page; they belong to Search settings, which is the
  page that would show them.
- **Sessions** are per user (`/admin/users/{id}/sessions`); they belong to
  Users.
- **Account recovery** sits in Configuration, as the brief says, although its
  permission is `administer users`: it is a site-wide policy, not a person.
- **Your admin preferences** is new and personal (theme, language, nothing
  else), so it sits beside the Dashboard rather than in a group.

The full table of every route, its handler, template, permission and group is
[inventory.md](inventory.md).

### Landing pages

Every group has a landing page at `/admin/{slug}/overview` (for example
`/admin/structure/overview`). It lists every page in the group the viewer may
open, each with its title as a link and a one sentence description. It is the
way to browse the admin without JavaScript, the target of the group's
breadcrumb, and the first link in the group's sidebar list. A viewer who may
open nothing in a group gets a 404 for its landing page, the same answer a
page that does not exist gets.

`/admin/{slug}/overview` rather than `/admin/{slug}` because `/admin/content`
and `/admin/people` already are the content and user lists, and one pattern for
every group is easier to guess than two.

## 2. The registry

One table drives the sidebar, the landing pages, the breadcrumbs, the
dashboard and admin search, so they cannot drift apart. It lives in
`crates/kernel/src/routes/admin_nav.rs`.

A kernel entry has: the group, a title, a one sentence description, the
address, the gate its GET handler checks (a permission, any of several
permissions, or the site administrator), the plugin that must be enabled for
the page to exist, other path prefixes that belong to the page (so
`/admin/structure/tags/9/edit` is under Categories), search synonyms, and
whether it is shown in the sidebar (every page is on its landing page and in
search; "Add user" is not in the sidebar).

A plugin entry comes from `tap_admin_nav` (BL-162), unchanged: section, label,
href, permission and an optional dashboard card description. The section names
the group: a group's title (`Structure`, `Content`, …), case-insensitively, or
the pre-B199 name `System`, which maps to Configuration; any other name puts the
link in Extend. Its `card_description`, when set, is also its line on the
landing page and what search matches beside its label; the declaration itself
is unchanged, so no plugin needs rebuilding.

Filtering is the one function every surface calls: the viewer passes the
entry's gate, and its plugin is enabled. The sidebar and search are display
only; every route still checks its own permission.

## 3. Page anatomy

Every admin page has the same skeleton, drawn by `templates/page--admin.html`:

1. A skip link, the first focusable element, to `<main id="main">`.
2. A `header` (banner) with the Trovato mark linking to the Dashboard, the
   search form in a `search` landmark (`<form role="search">`), and the
   viewer's links: View site, Your preferences. Log out is a POST with a
   CSRF token, which the shell cannot mint on every page without expiring the
   tokens of forms open in other tabs (tokens are kept in a short list), so it
   sits on the Dashboard's "You" card and on Your preferences.
3. A `nav` labelled "Administration": the sidebar.
4. `main`, containing in order:
   - a breadcrumb `nav` labelled "Breadcrumb": Administration › Group ›
     parent page › this page, the last item `aria-current="page"` and not a
     link (omitted on the Dashboard);
   - exactly one `h1`, the page's title;
   - a one or two sentence intro saying what the page is for;
   - local tasks (View, Edit, Translate, Revisions) as a `nav` labelled
     "Page tasks" of plain links, the current one `aria-current="page"` — never
     `role="tablist"`, because each loads a page;
   - messages: status in `role="status"`, errors in `role="alert"`, each with
     a text prefix ("Done:", "Error:"), never colour alone, rendered once;
   - the page's own content;
   - for someone who may change who can do what (`administer users`), a
     "Who can do this?" link to `/admin/people/who-can?path=<this page>`,
     the last thing in `main` (section 11).

The `<title>` is `Page, Group, Site name` (`Page, Site name` on personal
pages). It is filled from the rendered `h1` after rendering, so the two cannot
disagree: the template writes the heading once, and the shell copies its text
into the title and the breadcrumb's last item.

A child template sets `{% block page_title %}`, optionally
`{% block page_intro %}` (otherwise the registry description is used) and
`{% block page_actions %}` (the primary "Add …" link beside the heading), and
puts the rest in `{% block content %}`.

`page--admin.html` is a complete document and no longer extends `base.html`,
so a site theme's `base.html`, `base.css` and `theme.css` never reach the
admin, and nothing the admin does changes front end output. One exception is
deliberate: a theme chosen as the *admin* theme on the Themes page (B108) has
the stylesheets in its `static/css/` linked after `admin.css`, so that setting
still restyles the admin (before, it did so through `base.html`). A theme can
also override `page--admin.html` itself.

Forms the front end shares with the admin (`templates/form/*.html`) pass their
words through `t`; outside an admin page `t` returns the English source, and
`admin_render()` is false, so front end output is byte for byte what it was
(`front_end_forms_unchanged_test` compares it with output recorded from
`main`).

## 4. Sidebar

Valid HTML: one list, whose items are the Dashboard link and one item per
group. A group item is an `h2` holding a `<button aria-expanded
aria-controls>` disclosure, followed by the group's own `ul`. The first link
in each group is its Overview (landing page). The current page's link carries
`aria-current="page"` and the found-dot marker (section 6); its group is always
open.

Collapsing is remembered per user (stored server side with the other admin
preferences, so the page renders in the right state with no flash). With
JavaScript off every group is open and the button is not rendered.

Below 48rem the sidebar is behind a "Menu" disclosure button in the header.
Nothing opens on hover.

## 5. Visual system

Plain CSS, in `static/css/admin.css`, with cascade layers in this order:
`reset, tokens, base, layout, components, utilities`. No framework, no build.

### Tokens

Three layers of custom properties:

- **Primitives**, from `assets/brand/BRAND.md`: ink `#221B16`, clay
  `#B14B2E`, clay light `#E08963`, peach `#FFD9A8`, cream `#FAF5EF`, and a warm
  neutral ramp derived from ink and cream.
- **Semantic**: `--surface`, `--surface-raised`, `--surface-sunken`, `--text`,
  `--text-muted`, `--border`, `--border-strong`, `--accent`,
  `--accent-text` (text on accent), `--link`, `--danger`, `--danger-surface`,
  `--success`, `--success-surface`, `--warning-surface`, `--focus`. Each is one
  `light-dark()` pair, so light and dark are defined in one place.
- **Component**: `--sidebar-surface`, `--button-surface`, `--field-border`,
  and so on, defined from the semantic tokens only.

Light and dark follow `prefers-color-scheme`; a per user override (System,
Light, Dark, in Your preferences) sets `color-scheme` on `<html>`.

Every text pair is at least 4.5:1 and every component boundary and the focus
ring at least 3:1 in both themes. A unit test
(`routes::admin_design_tests`) reads the token values out of the stylesheet and
computes every declared pair, so a token edit that breaks contrast fails CI.

### Type, space, size

- System fonts only, nothing downloaded. Headings use `ui-rounded` where the
  platform has it (it echoes the monoline rounded wordmark) and fall back to
  the system UI face; body text is the system UI face; numbers in tables are
  tabular.
- Type scale in rem, six steps: 0.875, 1, 1.125, 1.375, 1.75, 2.25. Body is
  1rem, line height 1.5 (1.25 for headings).
- Spacing on a 4px base: `--space-1` (0.25rem) to `--space-8` (3rem).
- Every pointer target is at least 24×24 CSS pixels; primary actions 44px tall.
- Focus: a 3px ring in `--focus` with a 2px offset on every focusable element,
  `:focus-visible`, never removed.

### Direction, motion, forced colours, zoom

- Logical properties only (`margin-inline-start`, `inset-inline-end`,
  `text-align: start`), so a right-to-left language needs no other
  stylesheet. A unit test fails on any physical `left`/`right` property in the
  admin CSS or in an admin template's inline style.
- `prefers-reduced-motion: reduce` turns every transition off.
- `forced-colors: active` uses system colours (`CanvasText`, `Highlight`,
  `LinkText`) for borders, the current page marker and focus.
- Readable at 400% zoom and at 320 CSS pixels wide with no horizontal scrolling
  of the page: the layout is one column below 48rem, tables scroll inside their
  own labelled region, and nothing has a fixed width.

## 6. Identity

Trovato is Italian for *found*, and the logo is a t holding a small clay dot.
The admin takes three things from that and nothing from anyone else:

- **Paper and clay.** The page is warm cream paper, content sits on white
  sheets with a hairline warm border and no drop shadow, and clay is the only
  accent: links, the primary button, the focus ring's partner. No blue, which
  is the colour of the CMS field (BRAND.md says why).
- **The found-dot.** The current page in the sidebar and the current
  breadcrumb are marked with a small clay dot, the same shape as the logo's,
  as well as `aria-current` and a heavier weight — a marker that is never
  colour alone.
- **A light rail, not a dark one.** The sidebar is a slightly sunken cream
  rail, not the dark slab of a typical dashboard; the header is a thin rule
  with the lockup, not a coloured bar.

Density is comfortable: one column of content, a readable measure (about 72
characters for prose), tables slightly tighter than prose. Icons, where there
are any, are inline SVG drawn for Trovato and always paired with visible text
or an accessible name.

## 7. Translation

Every admin string goes through one function, `t`, available to every template
whether or not `trovato_locale` is enabled. See
[translation.md](translation.md) for the function, placeholders, plurals,
context, the pseudo locales and the marker mode the tests use.

The admin language is the viewer's choice in Your preferences, else the site's
default language; `lang` and `dir` on `<html>` follow it.

## 8. Form pattern

**One presentation layer: the macros.** Every admin field is drawn by
`templates/admin/macros/form.html`. Hand-written admin forms call its macros
(`field`, `textarea`, `select`, `checkbox`, `choices`, `fieldset_start` and
`fieldset_end`, `details_start` and `details_end`, `effect`, `actions`,
`error_summary`).

A form built in Rust with an `/admin` action renders through
`templates/admin/form/*.html`. Those templates do nothing but map the element
onto the same macros. So the two kinds of form produce identical markup, and
a test (`admin_forms_test::hand_written_and_builder_fields_are_the_same_markup`)
compares them.

The alternative was moving every hand-written form onto the Rust builder. It
was rejected for three reasons:
- About forty forms with their own handlers would have been rewritten,
  validation and all, for no difference a person can see or hear.
- Several of those forms are grids or listings with inline controls, which
  the builder does not model.
- The macros keep each form's markup readable where it is.

The front end's form templates (`templates/form/*.html`) are not changed. A
form whose action is not under `/admin` renders exactly as before, and a
golden test proves it. No admin template may draw a field by hand:
`admin_forms_test::no_admin_form_is_left_outside_the_pattern` fails on a
hand-written `<label>` (other than a visually hidden one in a grid cell),
`<select>`, `<textarea>`, `<legend>` or `form-item` block, an inline `on…=`
handler, or a posting form without `novalidate`.

**The field.**
- The label is above the field and always visible; a placeholder is never
  the label.
- A hint sits under the label, `id="{field}-hint"`, and the field names it in
  `aria-describedby`.
- Required is said in words, " (required)" inside the label, so it is part of
  the field's name; "(optional)" is available for a form where most fields
  are required. There is no asterisk and no colour-only cue.
- One column.
- Widths are sized to the value: `width="4"`, `"10"`, `"20"` or `"30"`
  characters, or the full measure.
- Person fields carry `autocomplete` (username, email, new-password).
- Related radios and checkboxes sit in a `fieldset` with a `legend`
  (`choices`).
- Long or advanced settings are collapsed in native `details` and `summary`,
  which every keyboard and screen reader can open with no script. A
  collapsible fieldset from the form builder is drawn the same way, on the
  front end too; it used to open only from an `onclick` on its legend.

**Errors.**
- Forms carry `novalidate`; the server validates on submit.
- A failed submit shows the form again with an error summary first: a
  `role="alert"` box, focusable (`tabindex="-1"`), headed "There is a
  problem", with one link per problem.
- `admin-forms.js` moves focus to the summary. Each link moves focus to its
  field and scrolls its label, or its group's legend, into view. Without
  script the links are ordinary anchors to the field's id.
- The page `<title>` starts with the translated "Error:"
  (`admin_nav::finish_page` reads it from the summary).
- Each failed field has `aria-invalid="true"` and a message,
  `id="{field}-error"`, under its label, starting with a visually hidden
  "Error:"; `aria-describedby` names the hint and the message.
- A handler reports a problem as `routes::admin_form::FieldError::on(id,
  message)`, or `FieldError::form(message)` for the form as a whole. The
  message is a `UiMessage`, translated when shown. A form built in Rust uses
  `form::ValidationError`, which the macros read the same way.
- Nothing validates on blur or as you type.

**Saving.**
- The primary action comes first in the action bar and is the one filled
  button.
- Destructive actions are styled as danger and worded as what they do.
  Delete confirmation without script comes with the table pattern (section 9).
- On a form taller than the window, `admin-forms.js` makes the action bar
  sticky and sets the page's `scroll-padding-block-end` to the bar's height,
  so a focused field is never hidden behind it (WCAG 2.4.11).
- An unsaved changes warning appears only while a form differs from what was
  loaded, and never after it is submitted. Only forms built from the field
  pattern are watched; a listing's checkboxes are not.
- After a successful save the handler redirects, the status message shows,
  and focus moves to it.

**What this changes.** A settings form whose effect is not obvious ends with
one short, factual sentence above Save, led by "What this changes:"
(`effect`).

**Widgets.**
- A section list that can be dragged (compound fields) also has Move up and
  Move down buttons (WCAG 2.5.7).
- The permission grid is a table with column headers (roles) and row headers
  (permissions); each checkbox is named "{role}: {permission}".
- Weight and order are number fields; nothing needs a drag.
- On admin pages the media picker is a native modal `dialog`
  (`admin-media-picker.js`):
  - focus stays inside it, Escape closes it, and focus returns to the button
    that opened it;
  - the type filter, search and upload are labelled;
  - each file is a button named by its file name, with `aria-pressed` for the
    chosen one;
  - the result count is announced.
  The front end keeps its own picker.
- Words that scripts show come from a translated table the admin shell
  renders (`<template id="admin-text">`, read through `window.trovatoText`).

## 9. Table pattern

Every admin listing is drawn the same way. `routes::admin_list` reads the
query and `templates/admin/macros/list.html` draws it:

- **Table.** A `caption` (it may be visually hidden), `scope="col"` on each
  column header, and the row's name as a `th scope="row"`. A test fails on
  any admin table without them.
- **Filter.** A GET form named "Filter …", in a `search` landmark. It has a
  text field `q` and the listing's own selects. Because it is GET, it works
  without JavaScript and every filtered address can be shared. A "Clear
  filter" link appears once a filter is applied.
- **Sort.** A sortable column's header holds a button. Pressing it sorts by
  that column, ascending first, then the other way. The sorted header
  carries `aria-sort`. The buttons submit a small GET form that keeps the
  filters, and the filter form keeps the sort. The address names the column
  (`sort=title`, `sort=-title`). The SQL comes only from each listing's own
  list of allowed columns.
- **Count.** "N results" (and "page P of Q" when there are several) sits
  above the table. After a filter or sort the same words are put into a
  polite live region, so a screen reader hears what the filter found.
- **Pages.** The pager is a `nav` labelled for what it pages ("Pages of
  content"). The current page carries `aria-current="page"`, and every link
  keeps the filter and the sort. Fifty rows a page.
- **Empty.** With nothing at all: what is missing and the next action ("Add
  content"). With a filter that matches nothing: say so and offer the way
  back ("Clear filter").
- **Bulk actions.** Each row checkbox is named by its row ("Select About
  us"). A select-all box checks every row and shows "mixed" when some are
  checked. A polite live region counts the selection ("Selected: 3"). The
  action is a labelled select with an Apply button.
- **Row actions.** Visible text links and buttons whose names include the
  object: "Edit About us", "Delete About us", never a column of bare
  "Edit".
- **Asking first.** A destructive action asks on a page of its own
  (`routes::admin_confirm`), the same with JavaScript on or off. Its form
  posts to its usual address with `?confirm`; the layer keeps the fields in
  the session and shows `/admin/confirm`. That page names what will happen
  ("Delete About us? This cannot be undone.", or "Delete 3 selected items?"
  for a bulk delete) and offers the real button and Cancel. Cancel goes back
  to the page the form was on. The real button posts the same fields to the
  same address, so the handler, with its own permission and token checks, is
  unchanged. A post whose form token does not verify is never shown on the
  page; it goes to the handler, which refuses it.

  We chose a page over a script-driven dialog because the page works
  without JavaScript, is one pattern instead of two, and is reached and left
  like any other page. A modal dialog would still need the page for anyone
  without scripts.
- **Narrow screens.** Below 40rem a listing table lays each row out as a
  block. Each cell is shown under its column's name, and the sort buttons
  stay in a row above. `admin-lists.js` copies the column names onto the
  cells. Without it, the table keeps its own labelled scroll region, which
  WCAG 1.4.10 allows for data tables.

## 10. Search pattern

One search, two ways in. `/admin/search?q=` is a page that works with
JavaScript off; the command palette is a quicker way to the same results.
Both call `routes::admin_search_index::search`, so they cannot disagree, and
a test compares them link by link.

**What it finds**, in this order, each kind under its own heading:

| Group | What matches | Links to | Shown to whoever may open |
|---|---|---|---|
| Pages | a registry page's title, description, group and synonyms, in English and in the viewer's language | the page | that page |
| Settings | a setting field's label, hint and synonyms | the field itself (`/admin/config/site#site_slogan`) | the form it is on |
| Content | item titles; the 8 most recently changed, then "All matches in Content" | the item's edit form | `/admin/content` |
| People | account names and email addresses | the account's edit form | `/admin/people` |
| Plugins | the pages enabled plugins declare (label, section, card description), and the plugins page's rows by name and description | the page, or the plugin's row (`/admin/plugins#plugin-blog`) | that page, or `/admin/plugins` |
| Log messages | site log message text; the 8 newest | the site log filtered to it | `/admin/reports/log` |

The gate of each group is the gate of the page it links to, through the
same `offered_pages` the sidebar uses. Search never shows a record the
viewer could not reach by the sidebar, and every route still checks its own
permission. The queries run live, with the listings' own filters, so a
deleted item or a disabled plugin is gone from the next search. Setting
fields are a fixed list in `admin_search_index::SETTINGS`; a test opens
each form and checks that every anchor is a field id there.

**The results page.** A `search` landmark with the query, then "N results"
as a status, then one `section` per group, labelled by its `h2`. A group
with more matches than it shows ends with "All matches in Content" (or
People, or Log messages), which opens that listing filtered by the query.
With nothing found, it says so and suggests shorter or task words.

**The command palette** (`static/js/admin-palette.js`):
- **Opening.** The banner's "Quick search" button (`aria-haspopup="dialog"`),
  shown only when the script runs. `/` opens it when focus is not in a text
  field. Each viewer may turn `/` off (WCAG 2.1.4) and add Control+K
  (Command+K on a Mac) or Alt+K (Option+K) under "Search shortcuts" in Your
  preferences; those work in text fields too. The button's
  `aria-keyshortcuts` names the keys in force, and its `<kbd>` shows `/`.
- **The dialog.** A native `<dialog>` opened with `showModal()`, so the
  page behind is inert (the APG modal dialog pattern). It is labelled by its
  `h2`. Escape, the Close button and a click outside close it; focus goes
  back to whatever opened it.
- **The combobox** (ARIA 1.2, editable, list autocomplete). The input keeps
  focus. `aria-controls` names the listbox, `aria-expanded` says whether it
  has options, and `aria-activedescendant` names the active option, which
  also carries `aria-selected="true"`. The listbox holds one `group` per
  kind, labelled by its heading. Down and Up move through every option and
  wrap. Enter opens the active option, or with none active, submits to the
  results page. A polite status says "N results" after each answer.
- **The answers** come from `GET /admin/search/suggest?q=`, as JSON already
  in the viewer's language. It refuses anyone who may not use the
  administration (403). Typing waits 150ms between requests, and a late
  answer to an older query is dropped.

`aria-activedescendant` was chosen over moving focus into the list, so the
query can be edited at any time. It works in Chromium and WebKit, which the
browser checks run in. If a screen reader is found not to follow it, the
fallback is to move real focus to the options (each `tabindex="-1"`) and
keep typing routed to the input.

## 11. Dashboard and "Who can do this?"

**Panels.** Above the area cards, the Dashboard shows what needs the viewer
now (`routes::admin_dashboard`):

| Panel | Says | Shown to whoever may open |
|---|---|---|
| Your recent content | how many items the viewer wrote and how many are not published; the last five, linked as the content listing links them: the admin edit form for someone who may edit any content, otherwise the item's own edit form or page | anyone who has written something or may create content |
| Waiting for you | drafts to review; comments waiting for approval | `/admin/content/moderated`; `/admin/content/comments` |
| Scheduled tasks | whether the last cron run worked, failed or was skipped; plugin queue jobs that stopped after repeated failures | `/admin/reports/cron` |
| Recent errors | errors logged in the last 24 hours, the newest five, each linking to the log filtered to it | `/admin/reports/log` |

Each panel is a `section` labelled by its `h2`, with its state in one
sentence ("Nothing is waiting for you.", "3 errors in the last 24 hours.").
Numbers are words in a sentence, with plural forms per language, and never
a figure alone, a chart or a colour. A panel's gate is the gate of the page
it is about, through `offered_pages`, so the Dashboard offers nothing that
answers 403. The cards below are unchanged: one per area, one per plugin
card, Create and You.

**"Who can do this?"** Each admin page the registry knows ends with a link
to `/admin/people/who-can?path=<the page>`, for someone who holds
`administer users` (the Permissions page's gate). The page:
- works the answer out again from the path, with the registry's gate for
  that page (`admin_nav::placement`), so nothing in the address can change
  it, and accepts only a clean path under `/admin`;
- names the permission, or any of several, or "only site administrators";
- lists the roles that hold it, how many people each has (or "everyone
  signed in" for the built-in roles), and links to edit each role and to the
  Permissions page.

Without a path, it lists what opens every page, kernel and plugin, each
linking to its own answer. The page leaves itself out of its own link.

**The accessibility report** (Reports, `administer site`) lists images
without a text alternative, newest first, each linking to the file page
where one is set (for someone who may open files). It also shows the
viewer's own settings (theme, search shortcuts), with a link to change
them, and how this administration is checked.
