The administration is organised by task. Every admin page now belongs to one
of seven groups (Content, Structure, Appearance, People, Extend, Configuration,
Reports) or is the Dashboard, and that placement lives in one registry
(`routes::admin_nav`) that the sidebar, the group landing pages
(`/admin/content`, `/admin/structure`, …), the breadcrumbs, the Dashboard and
the new admin search all read. Before, the sidebar, the Dashboard cards and the
breadcrumbs each kept their own list. They had drifted apart: a page could be
in the sidebar and missing from the Dashboard, or named differently in each,
and a plugin's admin page landed wherever its URL put it. A plugin still
declares its pages as before and may name a group; one that names none goes to
Extend. No URL changed and no route's permission changed. The sidebar, the
landing pages and the search are display only: each lists only the pages the
viewer may open, and every page still checks its own permission.
`docs/admin/inventory.md` lists every admin route with its handler, template,
permission and group, and a test fails when a route is added without a row or
the row's group disagrees with the registry.

Every admin page has the same anatomy: a skip link, a banner holding the admin
search, a sidebar navigation labelled "Administration" whose groups open and
close (by keyboard too, remembered between pages), a breadcrumb ending at the
current page, and one `h1` that also leads the document title. A test checks
it on every page type. Below 48rem the sidebar sits behind a Menu button, and
no admin page scrolls sideways at 320 CSS pixels. The admin stylesheet is
rebuilt on design tokens with a light and a dark theme, uses logical properties
only, so a right-to-left language mirrors the whole layout, and draws a 3px
focus ring on everything focusable. Tests compute the contrast of every token
pair in both themes and refuse a physical property. Each person may choose a
theme (system, light, dark) and an administration language under the new Your
preferences page (`/admin/preferences`). The admin no longer loads the site
theme's `base.css`, whose body colour and link rules leaked into it and failed
contrast in dark mode. The active admin theme's own stylesheets are still
linked after the admin's.

Every word the admin shows goes through one translation function, `t`, which
handles placeholders, plurals by CLDR category and context. It is registered
whether or not the locale plugin is enabled, and it returns the English source
when there is no translation. Flash messages are stored as messages, not as
sentences, so they are translated in the language of the page that shows them,
and one that reports a failure (a bulk action that changed nothing or only
part of the selection, a plugin that could not be switched, a test email that
could not be sent) is shown as an error and announced at once, not labelled
"Done:".
A marker-mode test opens every admin page type and fails on any text,
`aria-label`, `title`, `placeholder` or `alt` that did not go through `t`.
`TROVATO_PSEUDO_LOCALES=1` adds an accented and a right-to-left pseudo
language for checking layouts. The form templates the front end shares with
the admin now use `t` as well; outside an admin page `t` returns its English
source, and a test proves those templates render byte for byte as before. On an
admin page the file widget's label now names its file chooser, which it did not
before.

A new CI job, "Admin accessibility", generates a site and runs axe (WCAG 2.0,
2.1 and 2.2, A and AA) on every page in the inventory in four modes: light,
dark, 320px wide and right to left. It also runs keyboard checks and snapshots
each page type's accessibility tree. `docs/admin/design.md`,
`docs/admin/accessibility.md` and `docs/admin/translation.md` describe the
design, the checks and how to translate the admin. An admin page now makes one
more query, to read the viewer's preferences, plus one site setting.
