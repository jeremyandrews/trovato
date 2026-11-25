A plugin's own `/admin/...` page now renders with the kernel's admin sidebar,
and a plugin can add its own sidebar link and dashboard card to it.

`routes::plugin_api::themed_response`, the path a plugin's themed page takes,
built the page context without ever setting `admin_nav`: only
`helpers::render_admin_template` and the themed error page did. An admin page
served by a plugin therefore rendered `page--admin.html` with an empty
sidebar, and a site worked around the gap by hand-copying the kernel's link
list into its own template, shown to anyone admitted to `/admin` whatever
permissions they actually held. All three paths now build the sidebar through
one shared helper (`routes::admin_nav::insert_context`), so a plugin's admin
page and a kernel one cannot drift apart, and the workaround is no longer
needed. The page also gets the same header a kernel admin page draws, from the
title the plugin returns with `ApiResponse::themed`.

The sidebar and dashboard were also fixed Rust arrays a plugin could not add
to. A plugin now declares its own links and cards from a new tap,
`tap_admin_nav`, mirroring `tap_perm` and `tap_menu`: a declaration collected
once at boot and merged into the sidebar and dashboard under the same
permission filter the kernel's own links use, so a viewer never sees a link
or a card for a screen they cannot open, and a disabled plugin contributes
neither.
