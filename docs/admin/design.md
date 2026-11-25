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
| Reports | `reports` | Recent log messages, Audit log, Cron, Search report (and, from Part 5, the accessibility check) |

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
is unchanged, so no plugin needs rebuilding. (Part 4 adds search keywords to the
declaration.)

Filtering is the one function every surface calls: the viewer passes the
entry's gate, and its plugin is enabled. The sidebar and search are display
only; every route still checks its own permission.

## 3. Page anatomy

Every admin page has the same skeleton, drawn by `templates/page--admin.html`:

1. A skip link, the first focusable element, to `<main id="main">`.
2. A `header` (banner) with the Trovato mark linking to the Dashboard, the
   search form in a `search` landmark (`<form role="search">`), and the
   viewer's links: View site, Your preferences, Log out.
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
   - the page's own content.

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
admin, and nothing the admin does changes front end output.

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

## 8. Form pattern (Part 2)

Label above the field, always visible; hint under the label, tied with
`aria-describedby`; required marked in text; one column; widths sized to the
value; related controls in a `fieldset` with a `legend`; advanced settings in
native `details`. On a failed submit the `<title>` starts with "Error:", an
error summary (`role="alert"`, "There is a problem", one link per error)
takes focus, and each failed field has `aria-invalid="true"` and its message
tied with `aria-describedby`. Part 2 picks whether every form moves onto the
Rust form builder or the macro is rebuilt to emit identical markup, and records
the choice here.

## 9. Table pattern (Part 3)

`caption`, `scope` on headers, sortable columns as buttons in `th` with
`aria-sort` on the sorted one, a polite live result count after filtering,
pagination as a labelled `nav` with the current page marked, a helpful empty
state, a GET filter form, bulk actions with named row checkboxes, and row
action names that include the object ("Edit About us").

## 10. Search pattern

Part 1 ships the server side: `/admin/search?q=` searches the registry (title,
description, group, synonyms), filtered by the viewer's permissions, with
results under headings by kind. The form is in the header on every page, in a
`search` landmark, and works with JavaScript off. Part 4 adds settings,
content, people, plugins and log messages to the index, and the command
palette (APG modal dialog with an ARIA 1.2 combobox) opened by a visible
Search button or `/`.
