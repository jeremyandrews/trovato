A plugin's own admin page is now found by words it declares for it, not only
by its label, section and card description.

Before this, `AdminNavDefinition` (declared through `tap_admin_nav`) carried a
section, a label, an href, a permission and an optional card description, and
admin search matched a plugin page only against those: a viewer searching for
a word the page is known by, but that never appears in its title or
description, found nothing.

`AdminNavDefinition` has a new field, `keywords: Vec<String>`, set through a
new builder, `.with_keywords(...)`, next to the existing `.card(...)`. The
kernel feeds a plugin's keywords into admin search and the quick search
palette exactly as it feeds a kernel admin page's own keywords: matched, not
translated, and never widening who sees the link, which a viewer's permission
still decides alone. `AdminNavDefinition` is also `#[non_exhaustive]` now, so
a future field needs no second breaking bump; a plugin that builds one must
use `new` and the builders rather than a struct literal.

`test_plugin_api`'s admin page, the fixture that already proved a plugin's own
link reaches the sidebar, now also declares two keywords, and a kernel test
finds its page by one of them that appears in none of its label, section or
card description.
