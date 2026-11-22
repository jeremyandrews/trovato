The admin sidebar shows Comments and Categories again when those plugins are
enabled. Its link table tested the plugin names `comments` and `categories`,
but enabled plugins are held as `trovato_comments` and `trovato_categories`.
The comparison is exact, so both screens were hidden from everyone, and the
sidebar's own tests passed the same wrong names and passed. The names are now
the real ones, and a test holds every plugin the sidebar names to the list the
route gate is registered from.

The dashboard has one card per administration area: content, comments, people,
roles and permissions, structure, categories, menus, URL aliases, gathers,
tiles, plugins, site settings and reports. It used to link to content types,
the add forms and the health check, so most screens could be reached only by
typing their address. Each card's links pass the same gate and plugin check as
the sidebar, so a delegated role is shown the screens it may open and no card
for the rest.

The admin styles are no longer inline. `page--admin.html` links
`/static/css/admin.css` and `base.html` links `/static/css/base.css`, the
component sheet (variables, buttons, tables, messages) that used to sit in a
`<style>` block there. Both links carry `?v=` and the kernel version. A site
theme that replaces `base.html` used to lose every admin style with it: on one
site the sidebar's `--gray-900` was undefined and it drew white on white.
`admin.css` now imports `base.css` into a cascade layer, and layered rules
lose to every unlayered rule, so on the stock theme the copy changes nothing.
On a theme that never links `base.css`, the admin screens still get the
variables and components they are drawn with, and the theme's own CSS still
wins wherever it sets something. Nothing changes on the stock theme. A theme
that overrides `page--admin.html` itself should keep the `admin.css` link in
its `head` block.

`GET /admin/content/comments/settings` serves the comment moderation screen,
which holds the settings form, under `administer comments`. The route took only
POST, so a GET on the settings address answered 405.
