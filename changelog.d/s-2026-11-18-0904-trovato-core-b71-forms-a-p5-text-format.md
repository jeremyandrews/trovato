A plugin rendering HTML a user wrote could not filter it: `FilterPipeline` was
reachable from kernel code only, so a plugin either escaped everything or
returned the user's HTML as written, and the kernel does not filter what a
plugin returns. A new host interface, `text-format`, exposes `filter-text`
(SDK `host::filter_text`), which runs the same pipeline the kernel renders a
field with, for a format the acting user may use by the rule the item forms
apply, and errors (`-70` not permitted, `-71` unknown, `-72` too large) rather
than return unfiltered text. The plugin API version is now 0.105.
