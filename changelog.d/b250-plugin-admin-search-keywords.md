A plugin's own admin page is now found by words it declares for it, not only
by its label, section and card description.

Before this, `AdminNavDefinition` (declared through `tap_admin_nav`) carried a
section, a label, an href, a permission and an optional card description, and
admin search matched a plugin page only against those: a viewer searching for
a word the page is known by, but that never appears in its title or
description, found nothing. The root cause: plugin admin links had no keyword
field, so search could match only their label (and section and card
description), while every kernel admin page carried keywords of its own.

`AdminNavDefinition` has a new field, `keywords: Vec<String>`, set through a
new builder, `.with_keywords(...)`, next to the existing `.card(...)`. The
kernel feeds a plugin's keywords into admin search and the quick search
palette exactly as it feeds a kernel admin page's own keywords: matched as
written (kernel keywords are not translated either), and never widening who sees the link, which a viewer's permission
still decides alone. `AdminNavDefinition` is also `#[non_exhaustive]` now, so
a future field needs no second breaking bump; a plugin that builds one must
use `new` and the builders rather than a struct literal. Each keyword is
trimmed and at most 20 are kept, each at most 255 bytes; a blank or longer
one, or any past the twentieth, is dropped with a warning, and the page keeps
its link, its card and its other keywords.

`test_plugin_api`'s admin page, the fixture that already proved a plugin's own
link reaches the sidebar, now also declares two keywords, and a kernel test
finds its page by one of them that appears in none of its label, section or
card description.
